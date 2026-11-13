use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::PathBuf,
    time::Duration,
};

use ruscello_core::{CategoryId, Source};
use ruscello_engine::{AddDownload, Url};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Input {
    Http(Url),
    Torrent(String),
    Dlc(PathBuf),
    Text(PathBuf),
    Invalid(String),
}

impl Input {
    fn path(path: PathBuf) -> Self {
        let name = path.to_string_lossy().into_owned();
        match path
            .extension()
            .and_then(|part| part.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("torrent") => Self::Torrent(name),
            Some("dlc") => Self::Dlc(path),
            Some("txt") => Self::Text(path),
            _ => Self::Invalid(name),
        }
    }

    fn url(value: &str) -> Option<Self> {
        let mut url = Url::parse(value).ok()?;
        if url.scheme() == "magnet"
            && url
                .query_pairs()
                .any(|(key, value)| key == "xt" && !value.is_empty())
        {
            return Some(Self::Torrent(value.to_owned()));
        }
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return None;
        }
        url.set_fragment(None);
        if url.path().to_ascii_lowercase().ends_with(".torrent") {
            return Some(Self::Torrent(url.into()));
        }
        Some(Self::Http(url))
    }

    pub(crate) fn label(&self) -> String {
        match self {
            Self::Http(url) => url.to_string(),
            Self::Torrent(value) | Self::Invalid(value) => value.clone(),
            Self::Dlc(path) | Self::Text(path) => path.to_string_lossy().into_owned(),
        }
    }

    pub(crate) fn key(&self) -> &'static str {
        match self {
            Self::Http(_) => "direct-link",
            Self::Torrent(_) => "torrent-input",
            Self::Dlc(_) => "dlc-input",
            Self::Text(_) => "link-list",
            Self::Invalid(_) => "invalid-input",
        }
    }
}

pub(crate) fn parse(text: &str) -> Vec<Input> {
    let mut unique = HashSet::new();
    text.lines()
        .flat_map(|line| {
            let line = line.trim().trim_matches(['"', '\'']);
            if line.is_empty() {
                return Vec::new();
            }
            if !line.contains("://") && !line.starts_with("magnet:") {
                let input = Input::path(PathBuf::from(line));
                if !matches!(input, Input::Invalid(_)) {
                    return vec![input];
                }
            }
            let links: Vec<_> = line
                .split_whitespace()
                .filter_map(|value| {
                    Input::url(
                        value.trim_matches(['<', '>', '(', ')', '[', ']', '"', '\'', ',', ';']),
                    )
                })
                .collect();
            if links.is_empty() {
                vec![Input::Invalid(line.to_owned())]
            } else {
                links
            }
        })
        .filter(|input| unique.insert(input.clone()))
        .take(1000)
        .collect()
}

#[derive(Debug, Clone)]
pub(crate) enum Probe {
    Pending,
    File(String, Option<u64>),
    Web,
    Media,
    Unavailable,
}

pub(crate) async fn probe(url: Url) -> Probe {
    match tokio::time::timeout(Duration::from_secs(8), ruscello_engine::inspect_http(url)).await {
        Ok(Ok(info)) => {
            let content = info
                .content_type
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let path = info.final_url.path().to_ascii_lowercase();
            if path.ends_with(".m3u8")
                || path.ends_with(".mpd")
                || content.contains("mpegurl")
                || content.contains("dash+xml")
            {
                Probe::Media
            } else if content.starts_with("text/html") {
                Probe::Web
            } else {
                Probe::File(info.filename, info.size)
            }
        }
        _ => Probe::Unavailable,
    }
}

pub(crate) async fn load_files(paths: Vec<PathBuf>) -> Vec<Input> {
    tokio::task::spawn_blocking(move || {
        paths
            .into_iter()
            .flat_map(|path| match Input::path(path) {
                Input::Text(path) => {
                    let parsed = std::fs::File::open(&path).ok().and_then(|file| {
                        let max = 1024 * 1024;
                        if !file.metadata().ok()?.is_file() {
                            return None;
                        }
                        let mut text = String::new();
                        file.take(max + 1).read_to_string(&mut text).ok()?;
                        (text.len() as u64 <= max)
                            .then(|| parse(text.trim_start_matches('\u{feff}')))
                    });
                    parsed.map_or_else(
                        || vec![Input::Invalid(path.to_string_lossy().into_owned())],
                        |items| {
                            items
                                .into_iter()
                                .filter(|entry| matches!(entry, Input::Http(_) | Input::Torrent(_)))
                                .collect()
                        },
                    )
                }
                input => vec![input],
            })
            .take(1000)
            .collect()
    })
    .await
    .unwrap_or_default()
}

#[derive(Debug, Default)]
pub(crate) struct Dialog {
    pub(crate) text: String,
    pub(crate) entries: Vec<Input>,
    pub(crate) files: Vec<Input>,
    pub(crate) text_lists: HashSet<PathBuf>,
    pub(crate) previews: HashMap<Url, Probe>,
    pub(crate) category: Option<CategoryId>,
    pub(crate) destination: Option<PathBuf>,
    pub(crate) paused: bool,
    pub(crate) submitting: bool,
}

impl Dialog {
    fn is_ready(&self, url: &Url) -> bool {
        matches!(
            self.previews.get(url),
            Some(Probe::File(..) | Probe::Unavailable)
        )
    }

    pub(crate) fn can_submit(&self) -> bool {
        !self.submitting
            && self.entries.iter().any(|input| match input {
                Input::Http(url) => self.is_ready(url),
                _ => false,
            })
    }

    pub(crate) fn refresh(&mut self) -> Vec<Url> {
        self.entries = parse(&self.text);
        let mut unique = HashSet::new();
        self.entries.extend(self.files.iter().cloned());
        self.entries.retain(|item| unique.insert(item.clone()));
        let mut urls = Vec::new();
        for entry in &self.entries {
            if let Input::Http(url) = entry
                && !self.previews.contains_key(url)
            {
                self.previews.insert(url.clone(), Probe::Pending);
                urls.push(url.clone());
            }
        }
        urls
    }

    pub(crate) fn new_text_lists(&mut self) -> Vec<PathBuf> {
        self.entries
            .iter()
            .filter_map(|input| match input {
                Input::Text(path) if self.text_lists.insert(path.clone()) => Some(path.clone()),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn requests(&self) -> Vec<AddDownload> {
        self.entries
            .iter()
            .filter_map(|entry| {
                let Input::Http(url) = entry else {
                    return None;
                };
                if !self.is_ready(url) {
                    return None;
                }
                let mut request = AddDownload::new(Source::Http {
                    url: url.to_string(),
                });
                request.destination = self.destination.clone();
                request.category = self.category;
                request.start_immediately = !self.paused;
                if let Some(Probe::File(filename, _)) = self.previews.get(url) {
                    request.filename_hint = Some(filename.clone());
                }
                Some(request)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_and_deduplicates_inputs() {
        let entries = parse(
            "Try <https://example.test/file.zip> and https://example.test/file.zip#fragment\nmagnet:?xt=urn:btih:ABC\nC:\\My Downloads\\test.torrent\nlinks.DLC\nlinks.txt\ninvalid",
        );
        assert_eq!(
            entries.iter().map(Input::key).collect::<Vec<_>>(),
            [
                "direct-link",
                "torrent-input",
                "torrent-input",
                "dlc-input",
                "link-list",
                "invalid-input"
            ]
        );
    }
}
