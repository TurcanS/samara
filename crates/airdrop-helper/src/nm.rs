//! NetworkManager over D-Bus (replaces nmcli). NetworkManager keeps owning the
//! Wi-Fi device throughout: we only ask it to disconnect, stop auto-connecting
//! while AirDrop has the radio, and connect again.

use crate::out::log;
use std::io;
use std::time::{Duration, Instant};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedObjectPath;

const SERVICE: &str = "org.freedesktop.NetworkManager";
const ROOT: &str = "/org/freedesktop/NetworkManager";
const SETTINGS_PATH: &str = "/org/freedesktop/NetworkManager/Settings";
const SETTINGS: &str = "org.freedesktop.NetworkManager.Settings";
const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const CONF_DIR: &str = "/run/NetworkManager/conf.d";
/// Runtime-only rule (gone at reboot, removed on teardown): keeps wlan0's MAC stable
/// while disconnected. NM randomizes it for scans, and injected frames go out through
/// the vif whose MAC equals their TA (E0.7b run 1: 0 frames of ours on air).
pub const BORROW_CONF: &str = "/run/NetworkManager/conf.d/90-airdrop-borrow.conf";
/// Runtime-only rule: without it NM/wpa_supplicant would adopt the GO netdev and
/// switch it to station.
pub const GO_CONF: &str = "/run/NetworkManager/conf.d/90-airdrop-go.conf";

// NMDeviceState
pub const STATE_UNMANAGED: u32 = 10;
pub const STATE_DISCONNECTED: u32 = 30;
pub const STATE_ACTIVATED: u32 = 100;
const STATE_FAILED: u32 = 120;

type Result<T> = zbus::Result<T>;

/// What borrow mode did to the Wi-Fi connection, as recorded in the state file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Paused {
    pub uuid: Option<String>,
    pub name: Option<String>,
    /// The device was not connected: only auto-connect was switched off.
    #[serde(default)]
    pub autoconnect_only: bool,
    /// The access point it was on (NetworkManager object path), to reconnect to it directly.
    #[serde(default)]
    pub ap: Option<String>,
}

pub struct Nm {
    conn: Connection,
}

impl Nm {
    pub fn connect() -> Result<Self> {
        Ok(Self { conn: Connection::system()? })
    }

    fn manager(&self) -> Result<Proxy<'_>> {
        Proxy::new(&self.conn, SERVICE, ROOT, SERVICE)
    }

    /// Uncached: we poll `State` in loops, and a cached proxy would only see a change if its
    /// PropertiesChanged signal arrived first.
    fn device(&self, iface: &str) -> Result<Proxy<'_>> {
        let path: OwnedObjectPath = self.manager()?.call("GetDeviceByIpIface", &(iface,))?;
        zbus::blocking::proxy::Builder::new(&self.conn)
            .destination(SERVICE)?
            .path(path)?
            .interface(DEVICE)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
    }

    pub fn device_state(&self, iface: &str) -> Result<u32> {
        self.device(iface)?.get_property("State")
    }

    /// (uuid, name) of the connection active on `iface`.
    pub fn active_connection(&self, iface: &str) -> Result<Option<(String, String)>> {
        let active: OwnedObjectPath = self.device(iface)?.get_property("ActiveConnection")?;
        if active.as_str() == "/" {
            return Ok(None);
        }
        let p = Proxy::new(&self.conn, SERVICE, active, ACTIVE)?;
        Ok(Some((p.get_property("Uuid")?, p.get_property("Id")?)))
    }

    /// The access point the device is connected to, if any.
    pub fn active_ap(&self, iface: &str) -> Option<String> {
        let device = self.device(iface).ok()?;
        let wireless = zbus::blocking::proxy::Builder::<Proxy>::new(&self.conn)
            .destination(SERVICE).ok()?.path(device.path().to_owned()).ok()?.interface(WIRELESS).ok()?
            .cache_properties(zbus::proxy::CacheProperties::No).build().ok()?;
        let ap: OwnedObjectPath = wireless.get_property("ActiveAccessPoint").ok()?;
        (ap.as_str() != "/").then(|| ap.to_string())
    }

    pub fn disconnect(&self, iface: &str) -> Result<()> {
        self.device(iface)?.call("Disconnect", &())
    }

    pub fn set_autoconnect(&self, iface: &str, on: bool) -> Result<()> {
        Ok(self.device(iface)?.set_property("Autoconnect", on)?)
    }

    pub fn set_managed(&self, iface: &str, on: bool) -> Result<()> {
        Ok(self.device(iface)?.set_property("Managed", on)?)
    }

    /// Re-read NetworkManager's configuration files (flag 1 = conf).
    fn reload_conf(&self) {
        if let Err(e) = self.manager().and_then(|m| m.call::<_, _, ()>("Reload", &(1u32,))) {
            log(&format!("nm: reload failed: {e}"));
        }
    }

    /// Activate `uuid` on `iface`, or with `None` whatever NetworkManager finds best
    /// (what `nmcli device connect` does), and wait until the device is connected.
    /// `ap`: the access point to use (skips the choice when NetworkManager still knows it).
    fn activate(&self, iface: &str, uuid: Option<&str>, ap: Option<&str>) -> Result<bool> {
        let device = self.device(iface)?;
        let none = OwnedObjectPath::try_from("/").expect("valid path");
        let connection = match uuid {
            Some(u) => Proxy::new(&self.conn, SERVICE, SETTINGS_PATH, SETTINGS)?.call("GetConnectionByUuid", &(u,))?,
            None => none.clone(),
        };
        let specific = ap.and_then(|a| OwnedObjectPath::try_from(a).ok()).unwrap_or_else(|| none.clone());
        let _: OwnedObjectPath = self.manager()?.call("ActivateConnection", &(&connection, device.path(), &specific))?;
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(45) {
            match device.get_property::<u32>("State")? {
                STATE_ACTIVATED => return Ok(true),
                // Back to rest after it had time to start: this attempt is over.
                STATE_DISCONNECTED | STATE_FAILED if started.elapsed() > Duration::from_secs(3) => return Ok(false),
                // Short steps: the device is often up 0.4-0.6 s after the call, and 250 ms
                // steps added ~200 ms to every reconnect (journal vs helper log, 2026-11-14).
                _ => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        Ok(false)
    }

    /// Give Wi-Fi back after borrow mode: the same connection (on the same access point
    /// first), then any, then auto-connect on. Never fails; NetworkManager keeps managing
    /// the device throughout.
    pub fn reconnect(&self, iface: &str, paused: &Paused) {
        // Auto-connect goes back on last. Switched on first, NetworkManager at once starts
        // its own pick (here another saved network); our request then interrupts that
        // association and wpa_supplicant often rescans: 3.0 s instead of 0.75 s (2026-11-14).
        if !paused.autoconnect_only && !self.device_state(iface).is_ok_and(|s| s == STATE_ACTIVATED) {
            self.connect_paused(iface, paused);
        }
        if let Err(e) = self.set_autoconnect(iface, true) {
            log(&format!("nm: autoconnect on failed: {e}"));
        }
    }

    fn connect_paused(&self, iface: &str, paused: &Paused) {
        let mut choices = vec![None];
        if let Some(uuid) = paused.uuid.as_deref() {
            choices.insert(0, Some(uuid));
        }
        for attempt in 0..3 {
            // right after the radio is freed NetworkManager may still call the device unavailable
            for uuid in &choices {
                // The access point it was on first (once), then let NetworkManager pick:
                // a vanished access point object makes the call fail, not the reconnect.
                let ap = paused.ap.as_deref().filter(|_| attempt == 0 && uuid.is_some());
                if ap.is_some() {
                    match self.activate(iface, *uuid, ap) {
                        Ok(true) => return,
                        Ok(false) => log("nm: connecting to the same access point did not succeed"),
                        Err(e) => log(&format!("nm: activate on the same access point failed: {e}")),
                    }
                }
                match self.activate(iface, *uuid, None) {
                    Ok(true) => return,
                    Ok(false) => log(&format!("nm: connecting ({}) did not succeed", uuid.unwrap_or("any network"))),
                    Err(e) => log(&format!("nm: activate ({}) failed: {e}", uuid.unwrap_or("any network"))),
                }
            }
            if attempt < 2 {
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
}

/// Write a runtime-only NetworkManager drop-in and make NetworkManager read it.
pub fn write_conf(nm: Option<&Nm>, path: &str, body: &str) -> io::Result<()> {
    std::fs::create_dir_all(CONF_DIR)?;
    std::fs::write(path, body)?;
    if let Some(nm) = nm {
        nm.reload_conf();
    }
    Ok(())
}

/// Remove a drop-in (missing is fine) and make NetworkManager forget it.
pub fn remove_conf(nm: Option<&Nm>, path: &str) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    }
    if let Some(nm) = nm {
        nm.reload_conf();
    }
    Ok(())
}
