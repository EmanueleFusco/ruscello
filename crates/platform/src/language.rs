//! Language selection and localized startup messages.

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
}
