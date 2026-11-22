//! What each Wi-Fi driver family needs for AirDrop, picked from the kernel driver bound to the
//! Wi-Fi interface. Start of the per-driver profiles (plan 2026-11-22); the mt7921e specifics in
//! owl.rs and session.rs move here step by step.

use crate::sys;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Family {
    /// MediaTek MT7921/MT7922 (`mt7921e`): our patched build adds HT injection and `awdl_ba`.
    Mt7921,
    /// Realtek rtw89 (`rtw89_8852be`, ...): stock driver honours injected HT/VHT rates.
    Rtw89,
    /// Anything else: stock behaviour.
    Other,
}

impl Family {
    pub fn of(iface: &str) -> (Self, String) {
        let driver = sys::driver_of(iface).unwrap_or_default();
        let family = if driver == "mt7921e" {
            Self::Mt7921
        } else if driver.starts_with("rtw89_") {
            Self::Rtw89
        } else {
            Self::Other
        };
        (family, driver)
    }

    /// The chip holds injected frames to a peer it has no station entry for: rtw89 sends them
    /// under the access point's own mac_id, 17 at a time every ~140 ms (0.2 MB/s). With the
    /// iPhone registered as a station of our GO (OWL `-g`) the same frames go out at 5.6 MB/s
    /// (2026-11-22, `probe/sessions/*-p0-A` vs `*-p0-B`).
    pub fn needs_station(self) -> bool {
        self == Self::Rtw89
    }
}
