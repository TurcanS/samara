//! User configuration (`~/.config/airdrop/config.toml`) and XDG paths. The file format
//! is the old client's: only settings that differ from the defaults are written, plus
//! the identity (`service_id`), so a better default in a new version reaches users.

use crate::util::{expand_user, home, hex, random_bytes};
use std::fmt;
use std::path::{Path, PathBuf};

pub const AWDL_CHANNELS: [u32; 4] = [6, 36, 44, 149];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Borrow, or shared when the station already sits on 149.
    Auto,
    Shared,
    Borrow,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Shared => "shared",
            Self::Borrow => "borrow",
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "auto" => Ok(Self::Auto),
            "shared" => Ok(Self::Shared),
            "borrow" => Ok(Self::Borrow),
            "exclusive" => Err("mode exclusive was removed: it takes Wi-Fi down. Use borrow".into()),
            other => Err(format!("mode must be one of auto, shared, borrow, got {other:?}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Auto,
    Fixed(u32),
}

impl Channel {
    pub fn parse(s: &str) -> Result<Self, String> {
        if s == "auto" {
            return Ok(Self::Auto);
        }
        match s.parse::<u32>() {
            Ok(c) if AWDL_CHANNELS.contains(&c) => Ok(Self::Fixed(c)),
            _ => Err(format!("channel must be auto or one of 6, 36, 44, 149, got {s:?}")),
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::Fixed(c) => write!(f, "{c}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Media {
    /// The iPhone converts photos to JPEG first.
    Compatible,
    /// HEIC/HEVC as they are.
    Original,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub name: String,
    pub model: String,
    pub download_dir: PathBuf,
    pub service_id: String,
    pub wifi_iface: String,
    pub mode: Mode,
    pub channel: Channel,
    pub port: u16,
    pub media: Media,
    pub known_senders: Vec<String>,
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var).filter(|v| !v.is_empty()).map_or_else(|| home().join(fallback), PathBuf::from)
}

pub fn config_path() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("airdrop").join("config.toml")
}

pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join("airdrop")
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .and_then(|h| h.trim().split('.').next().map(str::to_string))
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "Linux".into())
}

impl Config {
    fn defaults(service_id: String) -> Self {
        Self {
            name: hostname(),
            model: "Linux".into(),
            download_dir: home().join("Downloads").join("AirDrop"),
            service_id,
            wifi_iface: "wlan0".into(),
            mode: Mode::Auto,
            channel: Channel::Auto,
            port: 8771,
            media: Media::Compatible,
            known_senders: vec![],
        }
    }

    /// Read the file; on first run write one, so the random service id stays ours.
    pub fn load() -> Result<Self, String> {
        let path = config_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Self::defaults(hex(&random_bytes::<6>()));
                cfg.save().map_err(|e| format!("{}: {e}", path.display()))?;
                return Ok(cfg);
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let table: toml::Table = text.parse().map_err(|e| format!("{}: {e}", path.display()))?;
        let mut cfg = Self::defaults(String::new());
        let text_of = |k: &str, v: &toml::Value| v.as_str().map(str::to_string).ok_or_else(|| format!("{k} must be a string"));
        for (key, value) in &table {
            match key.as_str() {
                "name" => cfg.name = text_of(key, value)?,
                "model" => cfg.model = text_of(key, value)?,
                "download_dir" => cfg.download_dir = expand_user(&text_of(key, value)?),
                "service_id" => cfg.service_id = text_of(key, value)?,
                "wifi_iface" => cfg.wifi_iface = text_of(key, value)?,
                "mode" => cfg.mode = Mode::parse(&text_of(key, value)?)?,
                "channel" => {
                    cfg.channel = match value {
                        toml::Value::Integer(i) => Channel::parse(&i.to_string())?,
                        _ => Channel::parse(&text_of(key, value)?)?,
                    }
                }
                "port" => {
                    cfg.port = value.as_integer().and_then(|p| u16::try_from(p).ok()).ok_or("port must be a number 0-65535")?;
                }
                "media" => {
                    cfg.media = match text_of(key, value)?.as_str() {
                        "compatible" => Media::Compatible,
                        "original" => Media::Original,
                        other => return Err(format!("media must be compatible or original, got {other:?}")),
                    }
                }
                "known_senders" => {
                    cfg.known_senders = value.as_array().ok_or("known_senders must be a list of names")?
                        .iter().map(|v| v.as_str().map(str::to_string).ok_or("known_senders must be a list of names"))
                        .collect::<Result<_, _>>()?;
                }
                _ => {} // unknown keys are ignored, as before
            }
        }
        if cfg.service_id.is_empty() {
            // A hand-written file without one: make it permanent rather than new per run.
            cfg.service_id = hex(&random_bytes::<6>());
            cfg.save().map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Ok(cfg)
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let d = Self::defaults(self.service_id.clone());
        let mut lines = vec!["# AirDrop for Linux configuration (only changed settings are stored)".to_string()];
        let mut put = |k: &str, v: String| lines.push(format!("{k} = {v}"));
        if self.name != d.name {
            put("name", quote(&self.name));
        }
        if self.model != d.model {
            put("model", quote(&self.model));
        }
        if self.download_dir != d.download_dir {
            put("download_dir", quote(&tilde(&self.download_dir)));
        }
        put("service_id", quote(&self.service_id));
        if self.wifi_iface != d.wifi_iface {
            put("wifi_iface", quote(&self.wifi_iface));
        }
        if self.mode != d.mode {
            put("mode", quote(self.mode.as_str()));
        }
        if let Channel::Fixed(c) = self.channel {
            put("channel", c.to_string());
        }
        if self.port != d.port {
            put("port", self.port.to_string());
        }
        if self.media == Media::Original {
            put("media", quote("original"));
        }
        if !self.known_senders.is_empty() {
            put("known_senders", format!("[{}]", self.known_senders.iter().map(|s| quote(s)).collect::<Vec<_>>().join(", ")));
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, lines.join("\n") + "\n")?;
        std::fs::rename(&tmp, &path)
    }

    pub fn is_known(&self, sender: &str) -> bool {
        let s = sender.to_lowercase();
        self.known_senders.iter().any(|k| k.to_lowercase() == s)
    }

    pub fn add_known(&mut self, sender: &str) -> std::io::Result<()> {
        if !self.is_known(sender) {
            self.known_senders.push(sender.to_string());
            self.save()?;
        }
        Ok(())
    }

    pub fn remove_known(&mut self, sender: &str) -> std::io::Result<bool> {
        let s = sender.to_lowercase();
        let before = self.known_senders.len();
        self.known_senders.retain(|k| k.to_lowercase() != s);
        if self.known_senders.len() == before {
            return Ok(false);
        }
        self.save()?;
        Ok(true)
    }
}

/// A TOML basic string, written the way Python's json.dumps(ensure_ascii=False) did.
fn quote(s: &str) -> String {
    serde_json::to_string(s).expect("a string always serializes")
}

fn tilde(p: &Path) -> String {
    let h = home();
    match p.strip_prefix(&h) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}
