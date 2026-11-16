//! The AirDrop wire format, without any I/O policy: plist messages (`wire`), HTTP/1.1
//! framing as a byte-fed state machine (`http`), the cpio/dvzip archive a sender
//! streams (`cpio`) and the extractor a receiver writes files with (`extract`).
//!
//! Byte-compatible with the Python client it replaced
//! and with what iOS 27.2 sends (docs/PROTOCOL.md).

pub mod cpio;
pub mod extract;
pub mod http;
pub mod wire;
