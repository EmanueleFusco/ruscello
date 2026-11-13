use std::{fs, io, path::Path};

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

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Preferences {
    pub(crate) locale: Locale,
    pub(crate) appearance: Appearance,
    pub(crate) details: bool,
    pub(crate) columns: u8,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            locale: Locale::System,
            appearance: Appearance::System,
            details: true,
            columns: 0b1_1111,
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
