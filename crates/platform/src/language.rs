//! Language selection and localized startup messages.

use ruscello_core::{Category, DefaultCategory, FileType};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    English,
    Italian,
}

impl Language {
    pub fn system() -> Self {
        let locale = sys_locale::get_locale();
        Self::from_locale(locale.as_deref().unwrap_or_default())
    }

    pub fn from_locale(locale: &str) -> Self {
        match locale.split(['-', '_', '.']).next() {
            Some(code) if code.eq_ignore_ascii_case("it") => Self::Italian,
            _ => Self::English,
        }
    }

    pub fn logging_unavailable(self) -> &'static str {
        match self {
            Self::English => "Ruscello could not initialize its log directory.",
            Self::Italian => "Ruscello non ha potuto inizializzare la cartella dei log.",
        }
    }

    pub fn category_name(self, category: &Category) -> &str {
        match (self, category.default_kind()) {
            (Self::Italian, Some(DefaultCategory::Games)) => "Giochi",
            (Self::Italian, Some(DefaultCategory::MoviesAndTv)) => "Film e TV",
            (Self::Italian, Some(DefaultCategory::Music)) => "Musica",
            (Self::Italian, Some(DefaultCategory::Software)) => "Software",
            (Self::Italian, Some(DefaultCategory::Work)) => "Lavoro",
            _ => &category.name,
        }
    }

    pub fn file_type_name(self, file_type: FileType) -> &'static str {
        match (self, file_type) {
            (_, FileType::Video) => "Video",
            (_, FileType::Audio) => "Audio",
            (Self::English, FileType::Images) => "Images",
            (Self::Italian, FileType::Images) => "Immagini",
            (Self::English, FileType::Documents) => "Documents",
            (Self::Italian, FileType::Documents) => "Documenti",
            (Self::English, FileType::Archives) => "Archives",
            (Self::Italian, FileType::Archives) => "Archivi",
            (Self::English, FileType::Programs) => "Programs",
            (Self::Italian, FileType::Programs) => "Programmi",
            (Self::English, FileType::Torrents) => "Torrents",
            (Self::Italian, FileType::Torrents) => "Torrent",
            (Self::English, FileType::Other) => "Other",
            (Self::Italian, FileType::Other) => "Altro",
        }
    }

    pub fn disk_space_low(self) -> &'static str {
        match self {
            Self::English => "Download paused because there is not enough disk space.",
            Self::Italian => "Download in pausa per spazio su disco insufficiente.",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_italian_and_defaults_to_english() {
        assert_eq!(Language::from_locale("it-IT"), Language::Italian);
        assert_eq!(Language::from_locale("IT_IT.UTF-8"), Language::Italian);
        assert_eq!(Language::from_locale("de-DE"), Language::English);
        assert_eq!(Language::from_locale(""), Language::English);
    }

    #[test]
    fn localizes_built_in_categories_and_preserves_renamed_categories() {
        let mut category = Category {
            id: DefaultCategory::MoviesAndTv.id(),
            name: DefaultCategory::MoviesAndTv.english_name().into(),
            destination: None,
        };
        assert_eq!(Language::English.category_name(&category), "Movies & TV");
        assert_eq!(Language::Italian.category_name(&category), "Film e TV");
        category.name = "Personal".into();
        assert_eq!(Language::Italian.category_name(&category), "Personal");
        assert_eq!(
            Language::Italian.file_type_name(FileType::Archives),
            "Archivi"
        );
        assert_ne!(
            Language::Italian.disk_space_low(),
            Language::English.disk_space_low()
        );
    }
}
