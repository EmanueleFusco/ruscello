use std::{
    fs, io,
    num::{NonZeroU16, NonZeroU64},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct TorrentPreferences {
    pub(crate) port: String,
    pub(crate) dht: bool,
    pub(crate) pex: bool,
    pub(crate) upnp: bool,
    pub(crate) seed_ratio: String,
    pub(crate) seed_minutes: String,
    pub(crate) interface: String,
}

impl Default for TorrentPreferences {
    fn default() -> Self {
        Self {
            port: "6881".into(),
            dht: true,
            pex: true,
            upnp: true,
            seed_ratio: String::new(),
            seed_minutes: String::new(),
            interface: String::new(),
        }
    }
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
