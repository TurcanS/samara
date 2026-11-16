//! Watch Bluetooth LE for AirDrop adverts through BlueZ over D-Bus (no privileges, no
//! `bluetoothctl`, Wi-Fi untouched). The scan belongs to our D-Bus connection: BlueZ stops
//! it when the connection goes, however this process ends.
//!
//! An iPhone that opens its share sheet broadcasts an Apple advert of type 0x05
//! (iOS 27.2, docs/PROTOCOL.md): `05 12 | 40 | tag(3) | 00000000 | 03 | 4 x 2-byte hashes | 00`.
//! The hashes are prefixes of SHA-256 of the sender's phone number and e-mail addresses.

use std::collections::{BTreeSet, HashMap};
use std::time::Instant;
use tokio::sync::mpsc;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const BLUEZ: &str = "org.bluez";
const APPLE: u16 = 0x004c;

#[derive(Debug, Clone)]
pub struct Advert {
    pub when: Instant,
    /// The four contact hashes, hex, sorted.
    pub hashes: BTreeSet<String>,
}

impl Advert {
    /// True if any wanted hash is in this advert, or nothing is wanted.
    pub fn matches(&self, wanted: &BTreeSet<String>) -> bool {
        wanted.is_empty() || !self.hashes.is_disjoint(wanted)
    }
}

/// The type-0x05 item in an Apple manufacturer payload (a list of type, length, value).
pub fn parse_airdrop(payload: &[u8]) -> Option<BTreeSet<String>> {
    let mut i = 0;
    while i + 2 <= payload.len() {
        let (kind, len) = (payload[i], payload[i + 1] as usize);
        let body = payload.get(i + 2..i + 2 + len)?;
        if kind == 0x05 && len == 0x12 {
            let hashes: BTreeSet<String> = (9..17).step_by(2).map(|j| crate::util::hex(&body[j..j + 2])).collect();
            // all zero: a non-Apple sender's wake-up advert (ours looks like this)
            return (hashes.len() > 1 || !hashes.contains("0000")).then_some(hashes);
        }
        i += 2 + len;
    }
    None
}

fn apple_data(props: &HashMap<String, OwnedValue>) -> Option<Vec<u8>> {
    let md: HashMap<u16, OwnedValue> = props.get("ManufacturerData")?.try_clone().ok()?.try_into().ok()?;
    md.get(&APPLE)?.try_clone().ok()?.try_into().ok()
}

/// Adverts as they are heard. An error (no controller, no BlueZ) arrives as `Err` and
/// ends the stream.
pub fn watch() -> mpsc::Receiver<Result<Advert, String>> {
    let (tx, rx) = mpsc::channel(256);
    std::thread::spawn(move || {
        if let Err(e) = scan(&tx) {
            let _ = tx.blocking_send(Err(e));
        }
    });
    rx
}

fn scan(tx: &mpsc::Sender<Result<Advert, String>>) -> Result<(), String> {
    let err = |e: zbus::Error| e.to_string();
    let conn = Connection::system().map_err(|e| format!("no D-Bus system bus: {e}"))?;
    let objects: HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>> =
        Proxy::new(&conn, BLUEZ, "/", "org.freedesktop.DBus.ObjectManager").map_err(err)?
            .call("GetManagedObjects", &()).map_err(|e| format!("BlueZ is not running: {e}"))?;
    let adapter = objects.iter().find(|(_, ifaces)| ifaces.contains_key("org.bluez.Adapter1")).map(|(p, _)| p.clone())
        .ok_or("No default controller (no Bluetooth adapter)")?;
    let a = Proxy::new(&conn, BLUEZ, adapter.as_str(), "org.bluez.Adapter1").map_err(err)?;
    let _ = a.set_property("Powered", true);
    // DuplicateData: report every advert, not only the first per device.
    let filter: HashMap<&str, Value> = HashMap::from([("Transport", Value::from("le")), ("DuplicateData", Value::from(true))]);
    a.call::<_, _, ()>("SetDiscoveryFilter", &(filter,)).map_err(err)?;
    let props_rule = zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal).sender(BLUEZ).map_err(err)?
        .interface("org.freedesktop.DBus.Properties").map_err(err)?.member("PropertiesChanged").map_err(err)?
        .add_arg("org.bluez.Device1").map_err(err)?.build();
    let added_rule = zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal).sender(BLUEZ).map_err(err)?
        .interface("org.freedesktop.DBus.ObjectManager").map_err(err)?.member("InterfacesAdded").map_err(err)?.build();
    let changed = MessageIterator::for_match_rule(props_rule, &conn, Some(256)).map_err(err)?;
    let added = MessageIterator::for_match_rule(added_rule, &conn, Some(256)).map_err(err)?;
    a.call::<_, _, ()>("StartDiscovery", &()).map_err(err)?;
    let send = |data: Option<Vec<u8>>| -> bool {
        match data.as_deref().and_then(parse_airdrop) {
            Some(hashes) => tx.blocking_send(Ok(Advert { when: Instant::now(), hashes })).is_ok(),
            None => !tx.is_closed(),
        }
    };
    // New devices arrive as InterfacesAdded; known ones as PropertiesChanged.
    let added_tx = tx.clone();
    let added_thread = std::thread::spawn(move || {
        for msg in added.flatten() {
            let Ok((_, ifaces)) = msg.body().deserialize::<(OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>)>() else { continue };
            let data = ifaces.get("org.bluez.Device1").and_then(apple_data);
            if let Some(hashes) = data.as_deref().and_then(parse_airdrop) {
                if added_tx.blocking_send(Ok(Advert { when: Instant::now(), hashes })).is_err() {
                    return;
                }
            } else if added_tx.is_closed() {
                return;
            }
        }
    });
    for msg in changed.flatten() {
        let Ok((_, props, _)) = msg.body().deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>() else { continue };
        if !send(apple_data(&props)) {
            break; // nobody listens any more
        }
    }
    let _ = a.call::<_, _, ()>("StopDiscovery", &());
    drop(added_thread);
    Ok(())
}
