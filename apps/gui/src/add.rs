use std::{
    collections::{BTreeSet, HashMap, HashSet},
    io::Read,
    path::PathBuf,
    time::Duration,
};

use ruscello_core::{CategoryId, Source, TorrentSource};
use ruscello_engine::{
    AddDownload, Candidate, CandidateGroup, ContainerFile, EngineHandle, ImportedLink,
    ImportedPackage, MediaError, MediaInspection, MediaKind, MediaRequest, PageAnalysis,
    ResolveError, TorrentInspection, Url, is_dash_url, is_hls_url, is_manifest_url, links,
};
use tokio_util::sync::CancellationToken;

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
            Some("txt" | "m3u" | "m3u8" | "metalink" | "meta4") => Self::Text(path),
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
            let links: Vec<_> = links(line)
                .iter()
                .filter_map(|value| Input::url(value.as_str()))
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
    Hoster(String),
    Web(String, Vec<Candidate>),
    Media(Box<MediaInspection>),
    MediaMissingSidecar,
    Unavailable,
    Verification,
    AccountRequired,
}

pub(crate) async fn inspect_media(
    engine: EngineHandle,
    url: Url,
    cancellation: CancellationToken,
) -> Probe {
    match tokio::time::timeout(
        Duration::from_secs(20),
        engine.inspect_media(&url, &cancellation),
    )
    .await
    {
        Ok(Ok(info)) => Probe::Media(Box::new(info)),
        Ok(Err(ruscello_engine::EngineError::Media(MediaError::SidecarMissing(_)))) => {
            Probe::MediaMissingSidecar
        }
        _ => Probe::Unavailable,
    }
}

pub(crate) async fn probe(
    engine: EngineHandle,
    url: Url,
    cancellation: CancellationToken,
) -> Probe {
    if is_manifest_url(&url) {
        return inspect_media(engine, url, cancellation).await;
    }
    match tokio::time::timeout(
        Duration::from_secs(15),
        engine.inspect_link(&url, &cancellation),
    )
    .await
    {
        Ok(Ok(PageAnalysis::Page { title, candidates })) => {
            let has_likely = candidates
                .iter()
                .any(|candidate| candidate.group == CandidateGroup::Likely);
            if !has_likely
                && engine
                    .sidecar_status()
                    .await
                    .is_ok_and(|status| status.ytdlp.installed)
                && let Probe::Media(info) = inspect_media(engine, url, cancellation).await
            {
                return Probe::Media(info);
            }
            Probe::Web(title, candidates)
        }
        Ok(Err(
            err @ (ruscello_engine::EngineError::Resolver(ResolveError::VerificationRequired)
            | ruscello_engine::EngineError::Resolver(
                ResolveError::AccountRequired | ResolveError::Keyring,
            )),
        )) => probe_result(Err(err)),
        Ok(Ok(analysis)) => probe_result(Ok(analysis)),
        Ok(Err(_)) | Err(_) => {
            if engine
                .sidecar_status()
                .await
                .is_ok_and(|status| status.ytdlp.installed)
            {
                return inspect_media(engine, url, cancellation).await;
            }
            Probe::Unavailable
        }
    }
}

pub(crate) fn probe_result(result: Result<PageAnalysis, ruscello_engine::EngineError>) -> Probe {
    match result {
        Ok(PageAnalysis::Direct(candidate)) => Probe::File(candidate.filename, candidate.size),
        Ok(PageAnalysis::Hoster { plugin, .. }) => Probe::Hoster(plugin),
        Ok(PageAnalysis::Page { title, candidates }) => Probe::Web(title, candidates),
        Err(ruscello_engine::EngineError::Resolver(ResolveError::VerificationRequired)) => {
            Probe::Verification
        }
        Err(ruscello_engine::EngineError::Resolver(
            ResolveError::AccountRequired | ResolveError::Keyring,
        )) => Probe::AccountRequired,
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
                vec![Input::path(path)]
            })
            .take(1000)
            .collect()
    })
    .await
    .unwrap_or_default()
}

pub(crate) async fn import(
    engine: Option<EngineHandle>,
    path: PathBuf,
    allow_fallback: bool,
) -> (PathBuf, Result<Vec<ImportedPackage>, &'static str>) {
    let import = match engine {
        Some(engine) => engine.inspect_container(&path, allow_fallback).await,
        None => EngineHandle::import_links(&path, allow_fallback).await,
    };
    let result = match import {
        Ok(ContainerFile::Packages(packages)) => Ok(packages),
        Err(ruscello_engine::EngineError::Resolver(ResolveError::KeyService)) => {
            Err("dlc-key-failed")
        }
        _ => Err("import-failed"),
    };
    (path, result)
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
    pub(crate) containers: HashMap<PathBuf, Option<Result<Vec<ImportedPackage>, &'static str>>>,
    pub(crate) previews: HashMap<Url, Probe>,
    pub(crate) torrent_previews: HashMap<String, TorrentPreview>,
    pub(crate) torrent_files: HashMap<String, BTreeSet<usize>>,
    pub(crate) media_formats: HashMap<Url, String>,
    pub(crate) media_playlist_items: HashMap<Url, BTreeSet<usize>>,
    pub(crate) category: Option<CategoryId>,
    pub(crate) destination: Option<PathBuf>,
    pub(crate) paused: bool,
    pub(crate) submitting: bool,
    pub(crate) advanced: bool,
    pub(crate) connections: String,
    pub(crate) bandwidth: String,
    pub(crate) checksum: String,
    pub(crate) selected_links: HashSet<Url>,
    pub(crate) grabber_search: String,
    pub(crate) grabber_filter: Option<CandidateGroup>,
    pub(crate) collapsed_groups: HashSet<CandidateGroup>,
}

impl Dialog {
    fn is_ready(&self, url: &Url) -> bool {
        match self.previews.get(url) {
            Some(Probe::File(..) | Probe::Hoster(_) | Probe::Unavailable | Probe::Verification) => {
                true
            }
            Some(Probe::Media(info)) => {
                info.playlist_items.is_empty()
                    || self
                        .media_playlist_items
                        .get(url)
                        .is_some_and(|selected| !selected.is_empty())
            }
            _ => false,
        }
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
                Input::Dlc(path) | Input::Text(path) => {
                    self.containers.get(path).is_none_or(Option::is_none)
                }
                _ => false,
            })
            && (!self.selected_links.is_empty()
                || self.entries.iter().any(|input| match input {
                    Input::Http(url) => self.is_ready(url),
                    Input::Torrent(key) => self.is_torrent_submittable(key),
                    _ => false,
                }))
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
        self.media_formats
            .retain(|url, _| self.previews.contains_key(url));
        self.media_playlist_items
            .retain(|url, _| self.previews.contains_key(url));
        self.torrent_previews
            .retain(|key, _| unique.contains(&Input::Torrent(key.clone())));
        self.torrent_files
            .retain(|key, _| self.torrent_previews.contains_key(key));
        self.containers.retain(|path, _| {
            unique.contains(&Input::Dlc(path.clone()))
                || unique.contains(&Input::Text(path.clone()))
        });
        let candidates = self.candidate_urls();
        self.selected_links.retain(|url| candidates.contains(url));
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

    pub(crate) fn new_containers(&mut self) -> Vec<PathBuf> {
        self.entries
            .iter()
            .filter_map(|input| match input {
                Input::Text(path) | Input::Dlc(path) if !self.containers.contains_key(path) => {
                    self.containers.insert(path.clone(), None);
                    Some(path.clone())
                }
                _ => None,
            })
            .collect()
    }

    pub(crate) fn set_probe(&mut self, url: Url, result: Probe) {
        if !self.previews.contains_key(&url) {
            return;
        }
        if let Probe::Web(_, candidates) = &result {
            self.selected_links.extend(
                candidates
                    .iter()
                    .filter(|candidate| candidate.group == CandidateGroup::Likely)
                    .map(|candidate| candidate.url.clone()),
            );
        } else if let Probe::Media(info) = &result {
            self.media_formats
                .entry(url.clone())
                .or_insert_with(|| "best".to_owned());
            if !info.playlist_items.is_empty() {
                self.media_playlist_items
                    .entry(url.clone())
                    .or_insert_with(|| info.playlist_items.iter().map(|item| item.index).collect());
            }
        }
        self.previews.insert(url, result);
    }

    pub(crate) fn set_media_format(&mut self, url: &Url, format: String) {
        if self.previews.contains_key(url) {
            self.media_formats.insert(url.clone(), format);
        }
    }

    pub(crate) fn toggle_playlist_item(&mut self, url: &Url, index: usize) {
        let Some(selected) = self.media_playlist_items.get_mut(url) else {
            return;
        };
        if !selected.remove(&index) {
            selected.insert(index);
        }
    }

    pub(crate) fn toggle_all_playlist_items(&mut self, url: &Url, select_all: bool) {
        let Some(Probe::Media(info)) = self.previews.get(url) else {
            return;
        };
        let entry = self.media_playlist_items.entry(url.clone()).or_default();
        if select_all {
            *entry = info.playlist_items.iter().map(|item| item.index).collect();
        } else {
            entry.clear();
        }
    }

    pub(crate) fn set_import(
        &mut self,
        path: PathBuf,
        result: Result<Vec<ImportedPackage>, &'static str>,
    ) {
        if !self.containers.contains_key(&path) {
            return;
        }
        if let Ok(packages) = &result {
            self.selected_links.extend(
                packages
                    .iter()
                    .flat_map(|package| &package.links)
                    .map(|link| link.url.clone()),
            );
        }
        self.containers.insert(path, Some(result));
    }

    pub(crate) fn page_candidates(&self) -> impl Iterator<Item = (&str, &[Candidate])> {
        self.entries.iter().filter_map(|entry| {
            let Input::Http(url) = entry else {
                return None;
            };
            let Probe::Web(title, candidates) = self.previews.get(url)? else {
                return None;
            };
            Some((title.as_str(), candidates.as_slice()))
        })
    }

    pub(crate) fn imported_packages(&self) -> impl Iterator<Item = &ImportedPackage> {
        self.entries
            .iter()
            .filter_map(|entry| {
                let (Input::Text(path) | Input::Dlc(path)) = entry else {
                    return None;
                };
                self.containers.get(path)?.as_ref()?.as_ref().ok()
            })
            .flatten()
    }

    fn candidate_urls(&self) -> HashSet<Url> {
        self.page_candidates()
            .flat_map(|(_, candidates)| candidates.iter().map(|candidate| candidate.url.clone()))
            .chain(
                self.imported_packages()
                    .flat_map(|package| package.links.iter().map(|link| link.url.clone())),
            )
            .collect()
    }

    pub(crate) fn requests(&self, batch_name: String) -> Vec<(Option<String>, Vec<AddDownload>)> {
        let mut seen = HashSet::new();
        let mut groups = Vec::new();
        for package in self.imported_packages() {
            let requests = package
                .links
                .iter()
                .filter(|link| {
                    self.selected_links.contains(&link.url) && seen.insert(link.url.clone())
                })
                .filter_map(|link| self.imported_request(link))
                .collect::<Vec<_>>();
            if !requests.is_empty() {
                groups.push((Some(package.name.clone()), requests));
            }
        }
        for (title, candidates) in self.page_candidates() {
            let requests = candidates
                .iter()
                .filter(|candidate| {
                    self.selected_links.contains(&candidate.url)
                        && seen.insert(candidate.url.clone())
                })
                .filter_map(|candidate| {
                    let mut request = self.link_request(
                        &candidate.url,
                        Some(candidate.filename.clone()),
                        candidate.size,
                    )?;
                    if candidate.group == CandidateGroup::Media
                        && let Some(kind) = manifest_media_kind(&candidate.url)
                    {
                        request.source = Source::Media {
                            url: candidate.url.to_string(),
                        };
                        request.media = Some(MediaRequest {
                            kind,
                            ..MediaRequest::default()
                        });
                    }
                    Some(request)
                })
                .collect::<Vec<_>>();
            if !requests.is_empty() {
                groups.push((Some(title.to_owned()), requests));
            }
        }
        for entry in &self.entries {
            if let Input::Http(url) = entry
                && let Some(Probe::Media(info)) = self.previews.get(url)
                && !info.playlist_items.is_empty()
                && seen.insert(url.clone())
            {
                let selected = self.media_playlist_items.get(url);
                let selector = self
                    .media_formats
                    .get(url)
                    .cloned()
                    .unwrap_or_else(|| "best".to_owned());
                let selected_fmt = info.formats.iter().find(|fmt| fmt.id == selector);
                let playlist_requests = info
                    .playlist_items
                    .iter()
                    .filter(|item| selected.is_some_and(|set| set.contains(&item.index)))
                    .filter_map(|item| {
                        let Ok(item_url) = Url::parse(&item.url) else {
                            return None;
                        };
                        if !seen.insert(item_url.clone()) {
                            return None;
                        }
                        let mut req =
                            self.link_request(&item_url, Some(item.filename.clone()), None)?;
                        req.source = Source::Media {
                            url: item.url.clone(),
                        };
                        req.media = Some(Self::build_media_request(
                            info,
                            &selector,
                            selected_fmt,
                            item.title.clone(),
                        ));
                        Some(req)
                    })
                    .collect::<Vec<_>>();
                if !playlist_requests.is_empty() {
                    let package_name = info
                        .playlist_title
                        .clone()
                        .unwrap_or_else(|| info.title.clone());
                    groups.push((Some(package_name), playlist_requests));
                }
            }
        }
        let requests = self
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Input::Http(url) => {
                    if !self.is_ready(url) || !seen.insert(url.clone()) {
                        return None;
                    }
                    if let Some(Probe::Media(info)) = self.previews.get(url) {
                        let selector = self
                            .media_formats
                            .get(url)
                            .cloned()
                            .unwrap_or_else(|| "best".to_owned());
                        let selected_fmt = info.formats.iter().find(|fmt| fmt.id == selector);
                        let mut request =
                            self.link_request(url, Some(info.suggested_filename.clone()), None)?;
                        request.source = Source::Media {
                            url: url.to_string(),
                        };
                        request.media = Some(Self::build_media_request(
                            info,
                            &selector,
                            selected_fmt,
                            info.title.clone(),
                        ));
                        return Some(request);
                    }
                    let (filename, size) = match self.previews.get(url) {
                        Some(Probe::File(filename, size)) => (Some(filename.clone()), *size),
                        _ => (None, None),
                    };
                    let mut request = self.link_request(url, filename, size)?;
                    if matches!(self.previews.get(url), Some(Probe::Hoster(_))) {
                        request.source = Source::Hoster {
                            url: url.to_string(),
                        };
                    }
                    Some(request)
                }
                Input::Torrent(key) => {
                    if !self.is_torrent_submittable(key)
                        || Url::parse(key).is_ok_and(|url| !seen.insert(url))
                    {
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
            .collect::<Vec<_>>();
        if !requests.is_empty() {
            groups.push(((requests.len() > 1).then_some(batch_name), requests));
        }
        groups
    }

    fn imported_request(&self, link: &ImportedLink) -> Option<AddDownload> {
        let mut request = self.link_request(&link.url, link.filename.clone(), link.size)?;
        if matches!(request.source, Source::Http { .. }) {
            request.output_filename.clone_from(&link.filename);
            if request.checksum.is_none() {
                request.checksum.clone_from(&link.checksum);
            }
            if request.checksum.is_some() {
                request.mirrors.clone_from(&link.mirrors);
            }
        }
        Some(request)
    }

    fn build_media_request(
        info: &MediaInspection,
        selector: &str,
        selected_fmt: Option<&ruscello_engine::MediaFormat>,
        title: String,
    ) -> MediaRequest {
        let format_selector = selected_fmt
            .map(|fmt| fmt.preset.as_selector())
            .unwrap_or_else(|| selector.to_owned());
        let variant_uri = selected_fmt
            .filter(|fmt| {
                info.kind == ruscello_engine::MediaKind::Hls
                    && fmt.id != "best"
                    && fmt.id != "audio"
            })
            .and_then(|fmt| fmt.url.clone());
        let dash_video_id = selected_fmt
            .filter(|fmt| info.kind == ruscello_engine::MediaKind::Dash && fmt.id != "best")
            .map(|fmt| fmt.id.clone());
        let dash_audio_id = selected_fmt.and_then(|fmt| fmt.audio_format_id.clone());
        let headers = selected_fmt
            .map(|fmt| fmt.headers.clone())
            .unwrap_or_default();
        MediaRequest {
            kind: info.kind,
            format_selector: Some(format_selector),
            title: Some(title),
            headers,
            variant_uri,
            dash_video_id,
            dash_audio_id,
        }
    }

    fn link_request(
        &self,
        url: &Url,
        filename: Option<String>,
        size: Option<u64>,
    ) -> Option<AddDownload> {
        let source = match Input::url(url.as_str())? {
            Input::Http(url) => Source::Http { url: url.into() },
            Input::Torrent(key) => Source::Torrent(torrent_source(&key)),
            _ => return None,
        };
        let mut request = AddDownload::new(source);
        request.destination = self.destination.clone();
        request.category = self.category;
        request.start_immediately = !self.paused;
        request.max_connections = self.connections.trim().parse().ok();
        request.bandwidth = self.bandwidth.trim().parse().ok();
        request.checksum =
            (!self.checksum.trim().is_empty()).then(|| self.checksum.trim().to_owned());
        request.filename_hint = filename;
        request.size_hint = size;
        Some(request)
    }
}

fn manifest_media_kind(url: &Url) -> Option<MediaKind> {
    if is_hls_url(url) {
        Some(MediaKind::Hls)
    } else if is_dash_url(url) {
        Some(MediaKind::Dash)
    } else {
        None
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

    #[test]
    fn link_grabber_media_routing_distinguishes_manifests_from_direct_files() -> anyhow::Result<()>
    {
        let page_url = Url::parse("https://example.test/watch")?;
        let mp4_url = Url::parse("https://cdn.example.test/video.mp4")?;
        let hls_url = Url::parse("https://cdn.example.test/master.m3u8")?;
        let dash_url = Url::parse("https://cdn.example.test/manifest.mpd")?;
        let candidates = vec![
            Candidate {
                url: mp4_url.clone(),
                filename: "video.mp4".to_owned(),
                size: Some(1024),
                score: 50,
                group: CandidateGroup::Media,
                reasons: Vec::new(),
            },
            Candidate {
                url: hls_url.clone(),
                filename: "master.m3u8".to_owned(),
                size: None,
                score: 40,
                group: CandidateGroup::Media,
                reasons: Vec::new(),
            },
            Candidate {
                url: dash_url.clone(),
                filename: "manifest.mpd".to_owned(),
                size: None,
                score: 40,
                group: CandidateGroup::Media,
                reasons: Vec::new(),
            },
        ];
        let dialog = Dialog {
            entries: vec![Input::Http(page_url.clone())],
            previews: HashMap::from([(page_url, Probe::Web("Sample Page".to_owned(), candidates))]),
            selected_links: [mp4_url, hls_url, dash_url].into_iter().collect(),
            ..Dialog::default()
        };

        let groups = dialog.requests("Batch".to_owned());
        assert_eq!(groups.len(), 1);
        let reqs = &groups[0].1;
        assert_eq!(reqs.len(), 3);
        assert!(matches!(reqs[0].source, Source::Http { .. }));
        assert!(reqs[0].media.is_none());
        assert!(matches!(reqs[1].source, Source::Media { .. }));
        assert_eq!(reqs[1].media.as_ref().map(|m| m.kind), Some(MediaKind::Hls));
        assert!(matches!(reqs[2].source, Source::Media { .. }));
        assert_eq!(
            reqs[2].media.as_ref().map(|m| m.kind),
            Some(MediaKind::Dash)
        );
        Ok(())
    }
}
