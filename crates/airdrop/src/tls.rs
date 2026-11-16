//! The self-signed TLS identity used on both sides, through the system OpenSSL: the same
//! library the old client's `ssl` module used, so the handshake the iPhone accepted
//! (TLS 1.2+, X25519MLKEM768, no SNI, no ALPN) stays as it was.

use crate::config::data_dir;
use crate::util::env;
use openssl::asn1::Asn1Time;
use openssl::bn::{BigNum, MsbOption};
use openssl::hash::MessageDigest;
use openssl::pkey::PKey;
use openssl::rsa::Rsa;
use openssl::ssl::{SslAcceptor, SslConnector, SslFiletype, SslMethod, SslOptions, SslVerifyMode, SslVersion};
use openssl::x509::extension::BasicConstraints;
use openssl::x509::{X509Name, X509};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

/// Python's default for an SSLContext: forward-secret AEAD suites for TLS 1.2.
const CIPHERS: &str = "@SECLEVEL=2:ECDH+AESGCM:ECDH+CHACHA20:ECDH+AES:DHE+AES:!aNULL:!eNULL:!aDSS:!SHA1:!AESCCM";

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// `cert.pem` and `key.pem` in the data folder, made on first use (RSA-2048, ten years).
pub fn ensure_identity(common_name: &str) -> Result<(PathBuf, PathBuf), Error> {
    let dir = data_dir();
    let (cert_path, key_path) = (dir.join("cert.pem"), dir.join("key.pem"));
    if cert_path.exists() && key_path.exists() {
        // The private key must stay ours alone, whoever created the file.
        if key_path.metadata()?.permissions().mode() & 0o077 != 0 {
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
        }
        return Ok((cert_path, key_path));
    }
    let rsa = Rsa::generate(2048)?;
    let key = PKey::from_rsa(rsa.clone())?;
    let mut name = X509Name::builder()?;
    name.append_entry_by_text("CN", if common_name.is_empty() { "AirDrop" } else { common_name })?;
    let name = name.build();
    let mut serial = BigNum::new()?;
    serial.rand(159, MsbOption::MAYBE_ZERO, false)?;
    let mut b = X509::builder()?;
    b.set_version(2)?;
    let serial = serial.to_asn1_integer()?;
    b.set_serial_number(&serial)?;
    b.set_subject_name(&name)?;
    b.set_issuer_name(&name)?;
    b.set_pubkey(&key)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
    let (before, after) = (Asn1Time::from_unix(now - 86_400)?, Asn1Time::from_unix(now + 3650 * 86_400)?);
    b.set_not_before(&before)?;
    b.set_not_after(&after)?;
    b.append_extension(BasicConstraints::new().critical().build()?)?;
    b.sign(&key, MessageDigest::sha256())?;
    let cert = b.build();
    std::fs::create_dir_all(&dir)?;
    let mut f = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&key_path)?;
    f.write_all(&rsa.private_key_to_pem()?)?;
    std::fs::write(&cert_path, cert.to_pem()?)?;
    Ok((cert_path, key_path))
}

/// Server side. `AIRDROP_TLS_PROBE` (reverse-engineering experiments E4.3, never for
/// normal use): `13` = TLS 1.3 plus a CertificateRequest (`AIRDROP_TLS_CA`: a DER CA to
/// accept the client's chain with); any other value = TLS 1.2 at most, plus a
/// CertificateRequest, so the client's certificate is readable in a capture.
pub fn acceptor(common_name: &str) -> Result<SslAcceptor, Error> {
    let (cert, key) = ensure_identity(common_name)?;
    let mut b = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls_server())?;
    b.set_min_proto_version(Some(SslVersion::TLS1_2))?;
    b.set_cipher_list(CIPHERS)?;
    b.set_options(SslOptions::NO_COMPRESSION | SslOptions::IGNORE_UNEXPECTED_EOF);
    b.set_certificate_chain_file(&cert)?;
    b.set_private_key_file(&key, SslFiletype::PEM)?;
    b.set_verify(SslVerifyMode::NONE); // "Everyone": peers present self-signed certificates
    match env("AIRDROP_TLS_PROBE").as_deref() {
        None => {}
        Some("13") => {
            b.set_min_proto_version(Some(SslVersion::TLS1_3))?;
            b.set_verify(SslVerifyMode::PEER);
            match env("AIRDROP_TLS_CA") {
                Some(ca) => {
                    let ca = X509::from_der(&std::fs::read(ca)?)?;
                    b.cert_store_mut().add_cert(ca)?;
                    b.verify_param_mut().set_flags(openssl::x509::verify::X509VerifyFlags::PARTIAL_CHAIN)?;
                }
                None => b.set_default_verify_paths()?,
            }
        }
        Some(_) => {
            b.set_max_proto_version(Some(SslVersion::TLS1_2))?;
            b.set_verify(SslVerifyMode::PEER);
            b.set_default_verify_paths()?;
        }
    }
    Ok(b.build())
}

/// Client side: no verification (receivers are self-signed), no SNI; our certificate is
/// presented if the receiver asks for one.
pub fn connector(common_name: &str) -> Result<SslConnector, Error> {
    let (cert, key) = ensure_identity(common_name)?;
    let mut b = SslConnector::builder(SslMethod::tls_client())?;
    b.set_min_proto_version(Some(SslVersion::TLS1_2))?;
    b.set_cipher_list(CIPHERS)?;
    b.set_options(SslOptions::NO_COMPRESSION | SslOptions::IGNORE_UNEXPECTED_EOF);
    b.set_verify(SslVerifyMode::NONE);
    b.set_certificate_chain_file(&cert)?;
    b.set_private_key_file(&key, SslFiletype::PEM)?;
    Ok(b.build())
}

/// TLS facts of a connection for `--dump`, and the peer's certificate (DER) if it sent one.
pub fn describe(ssl: &openssl::ssl::SslRef) -> (serde_json::Value, Option<Vec<u8>>) {
    let cipher = ssl.current_cipher().map(|c| serde_json::json!([c.name(), c.version(), c.bits().secret]));
    let cert = ssl.peer_certificate().and_then(|c| c.to_der().ok());
    let facts = serde_json::json!({
        "version": ssl.version_str(),
        "cipher": cipher,
        "alpn": ssl.selected_alpn_protocol().map(|a| String::from_utf8_lossy(a).into_owned()),
        "compression": serde_json::Value::Null,
        "peer_certificate_bytes": cert.as_ref().map_or(0, Vec::len),
    });
    (facts, cert)
}
