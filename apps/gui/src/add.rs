use std::{
    collections::{BTreeSet, HashMap, HashSet},
    io::Read,
    path::PathBuf,
    time::Duration,
};

use ruscello_core::{CategoryId, Source, TorrentSource};
use ruscello_engine::{AddDownload, TorrentInspection, Url};

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

fn read_link_shortcut(path: &std::path::Path) -> Option<Input> {
    const MAX_BYTES: u64 = 64 * 1024;
    let file = std::fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_BYTES + 1).read_to_string(&mut text).ok()?;
    if text.len() as u64 > MAX_BYTES {
        return None;
    }
    let text = text.trim_start_matches('\u{feff}');
    let mut internet_shortcut = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            internet_shortcut = line.eq_ignore_ascii_case("[InternetShortcut]");
        }
        if internet_shortcut
            && let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("url")
        {
            return Input::url(value.trim());
        }
    }
    None
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
            .flat_map(|path| {
                if let Some(input) = path.to_str().and_then(Input::url) {
                    return vec![input];
                }
                if path
                    .extension()
                    .and_then(|part| part.to_str())
                    .is_some_and(|part| part.eq_ignore_ascii_case("url"))
                {
                    return vec![
                        read_link_shortcut(&path)
                            .unwrap_or_else(|| Input::Invalid(path.to_string_lossy().into_owned())),
                    ];
                }
                match Input::path(path) {
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
                                    .filter(|entry| {
                                        matches!(entry, Input::Http(_) | Input::Torrent(_))
                                    })
                                    .collect()
                            },
                        )
                    }
                    input => vec![input],
                }
            })
            .take(1000)
            .collect()
    })
    .await
    .unwrap_or_default()
}

#[derive(Debug, Clone)]
pub(crate) enum TorrentPreview {
    Pending,
    Ready(Box<TorrentInspection>),
    Failed(String),
}

pub(crate) fn torrent_source(value: &str) -> TorrentSource {
    if value.starts_with("magnet:") {
        TorrentSource::Magnet(value.to_owned())
    } else if Url::parse(value)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
    {
        TorrentSource::Url(value.to_owned())
    } else {
        TorrentSource::File(PathBuf::from(value))
    }
}

#[derive(Debug, Default)]
pub(crate) struct Dialog {
    pub(crate) text: String,
    pub(crate) entries: Vec<Input>,
    pub(crate) files: Vec<Input>,
    pub(crate) text_lists: HashSet<PathBuf>,
    pub(crate) previews: HashMap<Url, Probe>,
    pub(crate) torrent_previews: HashMap<String, TorrentPreview>,
    pub(crate) torrent_files: HashMap<String, BTreeSet<usize>>,
    pub(crate) category: Option<CategoryId>,
    pub(crate) destination: Option<PathBuf>,
    pub(crate) paused: bool,
    pub(crate) submitting: bool,
    pub(crate) advanced: bool,
    pub(crate) connections: String,
    pub(crate) bandwidth: String,
    pub(crate) checksum: String,
}

impl Dialog {
    fn is_ready(&self, url: &Url) -> bool {
        matches!(
            self.previews.get(url),
            Some(Probe::File(..) | Probe::Unavailable)
        )
    }

    fn is_torrent_submittable(&self, key: &str) -> bool {
        match self.torrent_previews.get(key) {
            Some(TorrentPreview::Ready(info)) => {
                info.files.is_empty()
                    || self
                        .torrent_files
                        .get(key)
                        .is_some_and(|selected| !selected.is_empty())
            }
            Some(TorrentPreview::Failed(_)) => {
                !matches!(torrent_source(key), TorrentSource::File(_))
            }
            Some(TorrentPreview::Pending) | None => {
                matches!(torrent_source(key), TorrentSource::Magnet(_))
            }
        }
    }

    pub(crate) fn can_submit(&self) -> bool {
        !self.submitting
            && self.valid_options()
            && !self.entries.iter().any(|input| match input {
                Input::Http(url) => matches!(self.previews.get(url), None | Some(Probe::Pending)),
                Input::Torrent(key) => {
                    !matches!(torrent_source(key), TorrentSource::Magnet(_))
                        && matches!(
                            self.torrent_previews.get(key),
                            None | Some(TorrentPreview::Pending)
                        )
                }
                _ => false,
            })
            && self.entries.iter().any(|input| match input {
                Input::Http(url) => self.is_ready(url),
                Input::Torrent(key) => self.is_torrent_submittable(key),
                _ => false,
            })
    }

    fn valid_options(&self) -> bool {
        let connections = self.connections.trim();
        let checksum = self.checksum.trim();
        let bandwidth = self.bandwidth.trim();
        (connections.is_empty() || connections.parse::<std::num::NonZeroUsize>().is_ok())
            && (bandwidth.is_empty() || bandwidth.parse::<std::num::NonZeroU64>().is_ok())
            && (checksum.is_empty() || ruscello_engine::parse_checksum(checksum).is_ok())
    }

    pub(crate) fn refresh(&mut self) -> (Vec<Url>, Vec<String>) {
        self.entries = parse(&self.text);
        let mut unique = HashSet::new();
        self.entries.extend(self.files.iter().cloned());
        self.entries.retain(|item| unique.insert(item.clone()));
        self.previews
            .retain(|url, _| unique.contains(&Input::Http(url.clone())));
        self.torrent_previews
            .retain(|key, _| unique.contains(&Input::Torrent(key.clone())));
        self.torrent_files
            .retain(|key, _| self.torrent_previews.contains_key(key));
        let mut urls = Vec::new();
        let mut torrents = Vec::new();
        for entry in &self.entries {
            match entry {
                Input::Http(url) if !self.previews.contains_key(url) => {
                    self.previews.insert(url.clone(), Probe::Pending);
                    urls.push(url.clone());
                }
                Input::Torrent(key) if !self.torrent_previews.contains_key(key) => {
                    self.torrent_previews
                        .insert(key.clone(), TorrentPreview::Pending);
                    torrents.push(key.clone());
                }
                _ => {}
            }
        }
        (urls, torrents)
    }

    pub(crate) fn apply_torrent_inspection(
        &mut self,
        key: String,
        result: Result<TorrentInspection, String>,
    ) {
        if !self.torrent_previews.contains_key(&key) {
            return;
        }
        match result {
            Ok(info) => {
                self.torrent_files.entry(key.clone()).or_insert_with(|| {
                    info.files
                        .iter()
                        .filter(|file| file.selected)
                        .map(|file| file.index)
                        .collect()
                });
                self.torrent_previews
                    .insert(key, TorrentPreview::Ready(Box::new(info)));
            }
            Err(error) => {
                self.torrent_previews
                    .insert(key, TorrentPreview::Failed(error));
            }
        }
    }

    pub(crate) fn toggle_torrent_file(&mut self, key: &str, index: usize) {
        let Some(selected) = self.torrent_files.get_mut(key) else {
            return;
        };
        if !selected.remove(&index) {
            selected.insert(index);
        }
    }

    pub(crate) fn toggle_all_torrent_files(&mut self, key: &str, select_all: bool) {
        let Some(TorrentPreview::Ready(info)) = self.torrent_previews.get(key) else {
            return;
        };
        let entry = self.torrent_files.entry(key.to_owned()).or_default();
        if select_all {
            *entry = info.files.iter().map(|file| file.index).collect();
        } else {
            entry.clear();
        }
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
            .filter_map(|entry| match entry {
                Input::Http(url) => {
                    if !self.is_ready(url) {
                        return None;
                    }
                    let mut request = AddDownload::new(Source::Http {
                        url: url.to_string(),
                    });
                    request.destination = self.destination.clone();
                    request.category = self.category;
                    request.start_immediately = !self.paused;
                    request.max_connections = self.connections.trim().parse().ok();
                    request.bandwidth = self.bandwidth.trim().parse().ok();
                    request.checksum =
                        (!self.checksum.trim().is_empty()).then(|| self.checksum.trim().to_owned());
                    if let Some(Probe::File(filename, _)) = self.previews.get(url) {
                        request.filename_hint = Some(filename.clone());
                    }
                    Some(request)
                }
                Input::Torrent(key) => {
                    if !self.is_torrent_submittable(key) {
                        return None;
                    }
                    let mut request = AddDownload::new(Source::Torrent(torrent_source(key)));
                    request.destination = self.destination.clone();
                    request.category = self.category;
                    request.start_immediately = !self.paused;
                    request.bandwidth = self.bandwidth.trim().parse().ok();
                    if let Some(TorrentPreview::Ready(info)) = self.torrent_previews.get(key) {
                        request.filename_hint = Some(info.name.clone());
                        request.cached_torrent_bytes = info.torrent_bytes.clone();
                        if !info.files.is_empty()
                            && let Some(selected) = self.torrent_files.get(key)
                            && selected.len() < info.files.len()
                        {
                            request.only_files = Some(selected.iter().copied().collect());
                        }
                    }
                    Some(request)
                }
                _ => None,
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

    #[tokio::test]
    async fn imports_link_shortcuts_and_dropped_urls() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let shortcut = folder.path().join("example.url");
        std::fs::write(
            &shortcut,
            "[InternetShortcut]\r\nURL=https://example.test/file.zip\r\n",
        )?;
        let url = Url::parse("https://example.test/file.zip")?;
        assert_eq!(
            load_files(vec![shortcut, PathBuf::from(url.as_str())]).await,
            [Input::Http(url.clone()), Input::Http(url),]
        );
        Ok(())
    }
}
