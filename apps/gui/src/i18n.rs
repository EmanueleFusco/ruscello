use std::sync::Arc;

use fluent_bundle::{FluentArgs, FluentResource, concurrent::FluentBundle};
use ruscello_core::{Category, DefaultCategory, FileType, Status};
use ruscello_platform::language::Language;

use crate::preferences::Locale;

const EN: &str = include_str!("../i18n/en.ftl");
const IT: &str = include_str!("../i18n/it.ftl");

#[derive(Clone)]
pub(crate) struct Strings {
    english: Arc<FluentBundle<FluentResource>>,
    italian: Arc<FluentBundle<FluentResource>>,
    language: Language,
}

impl Strings {
    pub(crate) fn new(locale: Locale) -> anyhow::Result<Self> {
        Ok(Self {
            english: Arc::new(bundle("en", EN)?),
            italian: Arc::new(bundle("it", IT)?),
            language: locale.language(),
        })
    }

    pub(crate) fn set_locale(&mut self, locale: Locale) {
        self.language = locale.language();
    }

    pub(crate) fn get(&self, key: &str) -> String {
        self.format(key, None)
    }

    pub(crate) fn with<'a>(
        &self,
        key: &str,
        values: impl IntoIterator<Item = (&'a str, String)>,
    ) -> String {
        let args = values.into_iter().collect::<FluentArgs<'_>>();
        self.format(key, Some(&args))
    }

    fn format(&self, key: &str, args: Option<&FluentArgs<'_>>) -> String {
        let language = match self.language {
            Language::English => &self.english,
            Language::Italian => &self.italian,
        };
        for bundle in [language, &self.english] {
            if let Some(value) = bundle.get_message(key).and_then(|message| message.value()) {
                let mut errors = Vec::new();
                let value = bundle.format_pattern(value, args, &mut errors);
                if errors.is_empty() {
                    return value.into_owned();
                }
                tracing::debug!(key, "Could not format localization message");
            }
        }
        key.to_owned()
    }

    pub(crate) fn category(&self, category: &Category) -> String {
        let key = match category.default_kind() {
            Some(DefaultCategory::Games) => "games",
            Some(DefaultCategory::MoviesAndTv) => "movies",
            Some(DefaultCategory::Music) => "music",
            Some(DefaultCategory::Software) => "software",
            Some(DefaultCategory::Work) => "work",
            None => return category.name.clone(),
        };
        self.get(key)
    }
}

fn bundle(locale: &str, source: &str) -> anyhow::Result<FluentBundle<FluentResource>> {
    let mut bundle = FluentBundle::new_concurrent(vec![locale.parse()?]);
    bundle.set_use_isolating(false);
    let resource = FluentResource::try_new(source.to_owned())
        .map_err(|(_, errors)| anyhow::anyhow!("Invalid Fluent resource: {errors:?}"))?;
    bundle
        .add_resource(resource)
        .map_err(|errors| anyhow::anyhow!("Duplicate Fluent messages: {errors:?}"))?;
    Ok(bundle)
}

pub(crate) fn file_type(kind: FileType) -> &'static str {
    match kind {
        FileType::Video => "video",
        FileType::Audio => "audio",
        FileType::Images => "images",
        FileType::Documents => "documents",
        FileType::Archives => "archives",
        FileType::Programs => "programs",
        FileType::Torrents => "torrents",
        FileType::Other => "other",
    }
}

pub(crate) fn status(status: Status) -> &'static str {
    match status {
        Status::Queued => "queued",
        Status::Resolving => "resolving",
        Status::WaitingUser => "waiting-user",
        Status::Downloading => "downloading",
        Status::Paused => "paused",
        Status::Seeding => "seeding",
        Status::PostProcessing => "post-processing",
        Status::Completed => "completed",
        Status::Failed { .. } => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn locales_have_identical_message_ids_and_switch_at_runtime() -> anyhow::Result<()> {
        fn keys(source: &str) -> BTreeSet<&str> {
            source
                .lines()
                .filter_map(|line| line.split_once(" = "))
                .filter(|(key, _)| !key.starts_with(char::is_whitespace))
                .map(|(key, _)| key)
                .collect()
        }
        assert_eq!(keys(EN), keys(IT));
        let mut strings = Strings::new(Locale::English)?;
        assert_eq!(strings.get("settings"), "Settings");
        strings.set_locale(Locale::Italian);
        assert_eq!(strings.get("settings"), "Impostazioni");
        Ok(())
    }
}
