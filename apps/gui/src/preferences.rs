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
    pub(crate) utp: bool,
    pub(crate) pex: bool,
    pub(crate) upnp: bool,
    pub(crate) lsd: bool,
    pub(crate) download_limit: String,
    pub(crate) upload_limit: String,
    pub(crate) seed_ratio: String,
    pub(crate) seed_minutes: String,
    pub(crate) max_peers: String,
    pub(crate) max_global_peers: String,
    pub(crate) interface: String,
    pub(crate) blocklist: String,
}

impl Default for TorrentPreferences {
    fn default() -> Self {
        Self {
            port: String::new(),
            dht: true,
            utp: false,
            pex: true,
            upnp: true,
            lsd: true,
            download_limit: String::new(),
            upload_limit: String::new(),
            seed_ratio: String::new(),
            seed_minutes: String::new(),
            max_peers: String::new(),
            max_global_peers: String::new(),
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
        let max_peers_per_torrent = parse_optional_nonzero_usize(&self.max_peers)?
            .or(Some(TorrentSettings::DEFAULT_MAX_PEERS_PER_TORRENT));
        let max_global_peers = parse_optional_nonzero_usize(&self.max_global_peers)?
            .unwrap_or(TorrentSettings::DEFAULT_MAX_GLOBAL_PEERS);
        let bind_interface = normalize_optional_str(&self.interface);
        let blocklist_source = normalize_optional_str(&self.blocklist);
        Some(TorrentSettings {
            listen_port,
            fallback_listen_port: port.is_empty(),
            enable_dht: self.dht,
            enable_utp: self.utp,
            enable_pex: self.pex,
            enable_upnp: self.upnp,
            enable_lsd: self.lsd,
            fastresume: true,
            download_limit_bps,
            upload_limit_bps,
            seed_ratio_millis,
            seed_time_limit_minutes,
            max_peers_per_torrent,
            max_global_peers,
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

// The high bit marks masks saved after the upload column was introduced.
const COLUMNS_WITH_UPLOAD: u8 = 1 << 7;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Preferences {
    pub(crate) locale: Locale,
    pub(crate) appearance: Appearance,
    pub(crate) details: bool,
    pub(crate) motion: bool,
    #[serde(deserialize_with = "deserialize_columns")]
    pub(crate) columns: u8,
    pub(crate) column_widths: [Option<f32>; crate::app::Column::COUNT],
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
    pub(crate) sidecars: ruscello_engine::SidecarSettings,
    pub(crate) dlc_fallback: bool,
    pub(crate) browser_helper: bool,
    pub(crate) auto_accept_hosters: bool,
    pub(crate) monitor_clipboard: bool,
    pub(crate) sidebar_width: Option<f32>,
    pub(crate) details_height: Option<f32>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            locale: Locale::System,
            dlc_fallback: false,
            browser_helper: true,
            auto_accept_hosters: false,
            monitor_clipboard: false,
            appearance: Appearance::System,
            details: true,
            motion: true,
            columns: COLUMNS_WITH_UPLOAD | 0b11_1111,
            column_widths: [None; crate::app::Column::COUNT],
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
            sidecars: ruscello_engine::SidecarSettings::default(),
            sidebar_width: None,
            details_height: None,
        }
    }
}

fn deserialize_columns<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
    let columns = u8::deserialize(deserializer)?;
    Ok(if columns & COLUMNS_WITH_UPLOAD == 0 {
        columns | COLUMNS_WITH_UPLOAD | crate::app::Column::Upload.mask()
    } else {
        columns
    })
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

    #[test]
    fn older_column_visibility_is_preserved_and_upload_can_be_hidden() -> anyhow::Result<()> {
        use crate::app::Column;

        let mut old: Preferences = serde_json::from_str(r#"{"columns":1}"#)?;
        assert_eq!(old.columns & Column::Size.mask(), Column::Size.mask());
        assert_eq!(old.columns & Column::Status.mask(), 0);
        assert_eq!(old.columns & Column::Upload.mask(), Column::Upload.mask());
        old.columns ^= Column::Upload.mask();
        let restored: Preferences = serde_json::from_slice(&serde_json::to_vec(&old)?)?;
        assert_eq!(restored.columns, old.columns);
        Ok(())
    }

    #[test]
    fn older_torrent_preferences_preserve_custom_limits() -> anyhow::Result<()> {
        let mut preferences: TorrentPreferences =
            serde_json::from_str(r#"{"max_peers":"24","download_limit":"262144"}"#)?;
        let settings = preferences
            .to_settings()
            .ok_or_else(|| anyhow::anyhow!("invalid old preferences"))?;
        assert_eq!(
            settings.max_peers_per_torrent.map(NonZeroUsize::get),
            Some(24)
        );
        assert_eq!(settings.max_global_peers.get(), 500);
        assert!(!settings.enable_utp);
        preferences.utp = true;
        preferences.max_global_peers = "48".into();
        let restored: TorrentPreferences =
            serde_json::from_slice(&serde_json::to_vec(&preferences)?)?;
        let settings = restored
            .to_settings()
            .ok_or_else(|| anyhow::anyhow!("invalid restored preferences"))?;
        assert!(settings.enable_utp);
        assert_eq!(settings.max_global_peers.get(), 48);
        preferences.max_global_peers = "0".into();
        assert!(preferences.to_settings().is_none());
        Ok(())
    }
}
