use std::{
    fs, io,
    num::{NonZeroU16, NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
};

use ruscello_engine::TorrentSettings;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct TorrentPreferences {
    pub(crate) port: String,
    pub(crate) dht: bool,
    pub(crate) pex: bool,
    pub(crate) upnp: bool,
    pub(crate) lsd: bool,
    pub(crate) download_limit: String,
    pub(crate) upload_limit: String,
    pub(crate) seed_ratio: String,
    pub(crate) seed_minutes: String,
    pub(crate) max_peers: String,
    pub(crate) interface: String,
    pub(crate) blocklist: String,
}

impl Default for TorrentPreferences {
    fn default() -> Self {
        Self {
            port: "6881".into(),
            dht: true,
            pex: true,
            upnp: true,
            lsd: true,
            download_limit: String::new(),
            upload_limit: String::new(),
            seed_ratio: String::new(),
            seed_minutes: String::new(),
            max_peers: String::new(),
            interface: String::new(),
            blocklist: String::new(),
        }
    }
}

impl TorrentPreferences {
    pub(crate) fn to_settings(&self) -> Option<TorrentSettings> {
        let port = self.port.trim();
        let listen_port = if port.is_empty() {
            6881
        } else {
            port.parse::<u16>().ok()?
        };
        let download_limit_bps = parse_optional_nonzero_u64(&self.download_limit)?;
        let upload_limit_bps = parse_optional_nonzero_u64(&self.upload_limit)?;
        let seed_ratio_millis = parse_optional_ratio_millis(&self.seed_ratio)?;
        let seed_time_limit_minutes = parse_optional_u64(&self.seed_minutes)?;
        let max_peers_per_torrent = parse_optional_nonzero_usize(&self.max_peers)?;
        let bind_interface = normalize_optional_str(&self.interface);
        let blocklist_source = normalize_optional_str(&self.blocklist);
        Some(TorrentSettings {
            listen_port,
            enable_dht: self.dht,
            enable_pex: self.pex,
            enable_upnp: self.upnp,
            enable_lsd: self.lsd,
            fastresume: true,
            download_limit_bps,
            upload_limit_bps,
            seed_ratio_millis,
            seed_time_limit_minutes,
            max_peers_per_torrent,
            bind_interface,
            blocklist_source,
            dht_bootstrap_addrs: None,
        })
    }
}

fn parse_optional_nonzero_u64(raw: &str) -> Option<Option<NonZeroU64>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        Some(None)
    } else {
        trimmed.parse::<NonZeroU64>().ok().map(Some)
    }
}

fn parse_optional_nonzero_usize(raw: &str) -> Option<Option<NonZeroUsize>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        Some(None)
    } else {
        trimmed.parse::<NonZeroUsize>().ok().map(Some)
    }
}

fn parse_optional_u64(raw: &str) -> Option<Option<u64>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        Some(None)
    } else {
        trimmed.parse::<u64>().ok().map(Some)
    }
}

fn parse_optional_ratio_millis(raw: &str) -> Option<Option<u32>> {
    let trimmed = raw.trim().replace(',', ".");
    if trimmed.is_empty() {
        return Some(None);
    }
    let ratio = trimmed.parse::<f64>().ok()?;
    if !ratio.is_finite() || ratio < 0.0 || ratio > 1_000_000.0 {
        return None;
    }
    Some(Some((ratio * 1000.0).round() as u32))
}

fn normalize_optional_str(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

use ruscello_platform::language::Language;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Locale {
    #[default]
    System,
    English,
    Italian,
}

impl Locale {
    pub(crate) fn language(self) -> Language {
        match self {
            Self::System => Language::system(),
            Self::English => Language::English,
            Self::Italian => Language::Italian,
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::English => "english",
            Self::Italian => "italian",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Renderer {
    #[default]
    Hardware,
    Software,
}

impl Renderer {
    pub(crate) fn backend(self) -> &'static str {
        match self {
            Self::Hardware => "wgpu",
            Self::Software => "tiny-skia",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Preferences {
    pub(crate) locale: Locale,
    pub(crate) appearance: Appearance,
    pub(crate) details: bool,
    pub(crate) motion: bool,
    pub(crate) columns: u8,
    pub(crate) renderer: Renderer,
    pub(crate) default_folder: Option<PathBuf>,
    pub(crate) temporary_folder: Option<PathBuf>,
    pub(crate) alternate_bandwidth: NonZeroU64,
    pub(crate) lan_enabled: bool,
    pub(crate) lan_port: NonZeroU16,
    pub(crate) lan_auth: bool,
    pub(crate) check_updates: bool,
    pub(crate) close_to_tray: bool,
    pub(crate) notifications: bool,
    pub(crate) autostart: bool,
    pub(crate) torrent: TorrentPreferences,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            locale: Locale::System,
            appearance: Appearance::System,
            details: true,
            motion: true,
            columns: 0b1_1111,
            renderer: Renderer::Hardware,
            default_folder: None,
            temporary_folder: None,
            alternate_bandwidth: NonZeroU64::MIN.saturating_add(1024 * 1024 - 1),
            lan_enabled: false,
            lan_port: NonZeroU16::MIN.saturating_add(9665),
            lan_auth: true,
            check_updates: true,
            close_to_tray: false,
            notifications: true,
            autostart: false,
            torrent: TorrentPreferences::default(),
        }
    }
}

impl Preferences {
    pub(crate) fn load(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn save(self, path: &Path) -> io::Result<()> {
        let directory = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
        fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(&mut file, &self)?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_round_trip_and_default_for_older_files() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("gui.json");
        assert_eq!(Preferences::load(&path)?.locale, Locale::System);
        fs::write(&path, r#"{"locale":"Italian"}"#)?;
        let mut preferences = Preferences::load(&path)?;
        assert!(preferences.details);
        preferences.details = false;
        preferences.appearance = Appearance::Dark;
        preferences.save(&path)?;
        let saved = Preferences::load(&path)?;
        assert_eq!(saved.locale, Locale::Italian);
        assert_eq!(saved.appearance, Appearance::Dark);
        assert!(!saved.details);
        Ok(())
    }
}
