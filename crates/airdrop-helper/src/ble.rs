//! The AirDrop BLE advert through BlueZ's LE advertising API over D-Bus (replaces
//! bluetoothctl and btmgmt). The advert lives exactly as long as our D-Bus
//! connection, so a killed helper cannot leave one behind.

use crate::out::log;
use crate::sys;
use std::collections::HashMap;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

const BLUEZ: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";
const ADV_MANAGER: &str = "org.bluez.LEAdvertisingManager1";
const ADVERT_PATH: &str = "/org/airdrop/advert0";
const APPLE: u16 = 0x004c;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Version {
    /// iOS 26/27 senders: 0x40, random tag, version 0x03.
    V3,
    /// The pre-iOS 16 form (experiment E1.2).
    V1,
}

/// Payload of the manufacturer data (after the 0x004c company id):
/// type 0x05, length 0x12, then the body. Zero hashes still wake "Everyone" receivers.
pub fn payload(version: Version) -> Result<Vec<u8>, String> {
    Ok(match version {
        Version::V1 => {
            let mut p = vec![0x05, 0x12];
            p.extend([0u8; 8]);
            p.push(0x01);
            p.extend([0u8; 8]);
            p.push(0x00);
            p
        }
        Version::V3 => {
            let mut tag = [0u8; 3];
            sys::random_bytes(&mut tag).map_err(|e| format!("no random advert tag: {e}"))?;
            let mut p = vec![0x05, 0x12, 0x40];
            p.extend(tag);
            p.extend([0u8; 4]);
            p.push(0x03);
            p.extend([0u8; 8]);
            p.push(0x00);
            p
        }
    })
}

struct Advert {
    data: Vec<u8>,
}

/// The object BlueZ reads: non-connectable ("broadcast", as an iPhone 17 sends
/// ADV_NONCONN_IND) at a 100 ms interval.
#[zbus::interface(name = "org.bluez.LEAdvertisement1")]
impl Advert {
    fn release(&self) {
        log("ble: BlueZ released the advertisement");
    }

    #[zbus(property, name = "Type")]
    fn kind(&self) -> String {
        "broadcast".into()
    }

    #[zbus(property, name = "ManufacturerData")]
    fn manufacturer_data(&self) -> HashMap<u16, Value<'static>> {
        HashMap::from([(APPLE, Value::from(self.data.clone()))])
    }

    #[zbus(property, name = "MinInterval")]
    fn min_interval(&self) -> u32 {
        100
    }

    #[zbus(property, name = "MaxInterval")]
    fn max_interval(&self) -> u32 {
        100
    }
}

pub struct Advertiser {
    conn: Connection,
    adapter: OwnedObjectPath,
}

impl Advertiser {
    /// Register the advert on the first adapter that can advertise.
    pub fn start(version: Version) -> Result<Self, String> {
        let conn = Connection::system().map_err(|e| format!("no system bus: {e}"))?;
        let adapter = find_adapter(&conn)?;
        match Proxy::new(&conn, BLUEZ, adapter.clone(), ADAPTER) {
            Ok(p) => {
                if let Err(e) = p.set_property("Powered", true) {
                    log(&format!("ble: power on failed: {e}"));
                }
            }
            Err(e) => log(&format!("ble: adapter proxy: {e}")),
        }
        let data = payload(version)?;
        log(&format!("ble: advertise broadcast, manufacturer 0x004c {}",
            data.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")));
        conn.object_server().at(ADVERT_PATH, Advert { data }).map_err(|e| e.to_string())?;
        {
            let manager = Proxy::new(&conn, BLUEZ, adapter.clone(), ADV_MANAGER).map_err(|e| e.to_string())?;
            let path = ObjectPath::try_from(ADVERT_PATH).map_err(|e| e.to_string())?;
            let options: HashMap<&str, Value<'_>> = HashMap::new();
            manager.call::<_, _, ()>("RegisterAdvertisement", &(&path, options)).map_err(|e| e.to_string())?;
        }
        Ok(Self { conn, adapter })
    }

    /// Unregister; dropping the connection would do the same, this is just prompt.
    pub fn stop(self) {
        if let Ok(manager) = Proxy::new(&self.conn, BLUEZ, self.adapter.clone(), ADV_MANAGER)
            && let Ok(path) = ObjectPath::try_from(ADVERT_PATH)
            && let Err(e) = manager.call::<_, _, ()>("UnregisterAdvertisement", &(&path,)) {
            log(&format!("ble: unregister failed: {e}"));
        }
    }
}

type ManagedObjects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

fn find_adapter(conn: &Connection) -> Result<OwnedObjectPath, String> {
    let root = Proxy::new(conn, BLUEZ, "/", "org.freedesktop.DBus.ObjectManager").map_err(|e| e.to_string())?;
    let objects: ManagedObjects = root.call("GetManagedObjects", &()).map_err(|e| format!("BlueZ not available: {e}"))?;
    let mut paths: Vec<_> = objects.iter().filter(|(_, i)| i.contains_key(ADV_MANAGER)).map(|(p, _)| p.clone()).collect();
    paths.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    paths.into_iter().next().ok_or_else(|| "no Bluetooth adapter can advertise".to_string())
}
