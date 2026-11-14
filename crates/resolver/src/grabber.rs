use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
    time::Duration,
};

use futures_util::{StreamExt, stream};
use regex::Regex;
use reqwest::{Response, Url, header};
use scraper::{Html, Selector};

use crate::{Builtin, Context, HostPlugin, http};

const MAX_CANDIDATES: usize = 400;
const MAX_PROBES: usize = 16;
static SCRIPT_LINK: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"(?i)["']((?:https?://|//|/)[^"'<>\s]+)["']"#).ok());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateGroup {
    Likely,
    OtherFiles,
    Media,
    LinkedPages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreReason {
    FileExtension,
    LargeFile,
    DownloadLabel,
    TitleMatch,
    KnownHoster,
    Attachment,
    Tracker,
    TinyFile,
    InterfaceAsset,
    SocialOrLogin,
}

impl ScoreReason {
    pub const fn weight(self) -> i16 {
        match self {
            Self::FileExtension => 30,
            Self::LargeFile => 25,
            Self::DownloadLabel => 14,
            Self::TitleMatch => 18,
            Self::KnownHoster => 40,
            Self::Attachment => 35,
            Self::Tracker => -100,
            Self::TinyFile => -22,
            Self::InterfaceAsset => -40,
            Self::SocialOrLogin => -45,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub url: Url,
    pub filename: String,
    pub size: Option<u64>,
    pub group: CandidateGroup,
    pub score: i16,
    pub reasons: Vec<ScoreReason>,
}

impl Candidate {
    fn new(url: Url) -> Self {
        let filename = filename(&url);
        Self {
            url,
            filename,
            size: None,
            group: CandidateGroup::LinkedPages,
            score: 0,
            reasons: Vec::new(),
        }
    }
    fn add(&mut self, reason: ScoreReason) {
        if !self.reasons.contains(&reason) {
            self.score += reason.weight();
            self.reasons.push(reason);
        }
    }
    fn classify(&mut self, media: bool, file: bool) {
        self.group = if media {
            CandidateGroup::Media
        } else if self.score >= 30 {
            CandidateGroup::Likely
        } else if file || self.reasons.contains(&ScoreReason::Attachment) {
            CandidateGroup::OtherFiles
        } else {
            CandidateGroup::LinkedPages
        };
    }
    fn size(&mut self, size: Option<u64>) {
        self.size = size;
        match size {
            Some(bytes) if bytes >= 10 * 1024 * 1024 => self.add(ScoreReason::LargeFile),
            Some(bytes) if bytes < 16 * 1024 => self.add(ScoreReason::TinyFile),
            _ => {}
        }
    }
}

#[derive(Debug, Clone)]
pub enum PageAnalysis {
    Direct(Candidate),
    Hoster {
        candidate: Candidate,
        plugin: String,
    },
    Page {
        title: String,
        candidates: Vec<Candidate>,
    },
}

impl PageAnalysis {
    pub(crate) fn hoster(url: Url, plugin: &str) -> Self {
        Self::Hoster {
            candidate: hoster_candidate(url, None, None),
            plugin: plugin.into(),
        }
    }

    pub(crate) fn direct(url: Url, size: Option<u64>) -> Self {
        Self::Direct(Self::direct_candidate(url, size))
    }

    fn direct_candidate(url: Url, size: Option<u64>) -> Candidate {
        let mut candidate = Candidate::new(url);
        candidate.size(size);
        candidate.classify(false, true);
        candidate
    }

    pub(crate) fn from_response(url: Url, response: &Response) -> Self {
        let mut candidate = Self::direct_candidate(url, response.content_length());
        candidate.filename = filename(response.url());
        if http::attachment(response) {
            candidate.add(ScoreReason::Attachment);
        }
        let mime = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|header| header.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        candidate.classify(
            stream_url(response.url()) || mime.contains("mpegurl") || mime.contains("dash+xml"),
            true,
        );
        Self::Direct(candidate)
    }
}

pub(crate) fn hoster_candidate(url: Url, name: Option<&str>, size: Option<u64>) -> Candidate {
    let mut candidate = Candidate::new(url);
    if let Some(name) = name {
        candidate.filename = name.to_owned();
    }
    candidate.add(ScoreReason::KnownHoster);
    if downloadable(&extension(&candidate.url))
        || candidate
            .filename
            .rsplit_once('.')
            .is_some_and(|(_, ext)| downloadable(&ext.to_ascii_lowercase()))
    {
        candidate.add(ScoreReason::FileExtension);
    }
    candidate.size(size);
    candidate.classify(false, true);
    candidate
}

pub fn analyze_html(base: &Url, html: &str, resources: &[Url]) -> PageAnalysis {
    analyze_with_filter(base, html, resources, &HashSet::new())
}

pub(crate) fn analyze_with_filter(
    base: &Url,
    html: &str,
    resources: &[Url],
    blocked: &HashSet<String>,
) -> PageAnalysis {
    let document = Html::parse_document(html);
    let title = http::first(&document, "meta[property='og:title']")
        .and_then(|meta| meta.attr("content"))
        .map(str::to_owned)
        .or_else(|| http::first(&document, "title").map(|element| element.text().collect()))
        .unwrap_or_else(|| base.host_str().unwrap_or_default().to_owned());
    let title = title.trim().chars().take(200).collect::<String>();
    let mut found = HashMap::new();
    let collection = Builtin::ALL
        .into_iter()
        .find(|plugin| plugin.matches(base) && plugin.collection(base));
    let mut add = |value: &str, label: &str, media: bool, attachment: bool| {
        let Some(url) = http::join(base, value).filter(|url| url != base) else {
            return;
        };
        if collection.is_some_and(|plugin| {
            Builtin::ALL
                .into_iter()
                .any(|other| other == plugin && other.matches(&url) && other.collection(&url))
        }) {
            return;
        }
        if found.len() >= MAX_CANDIDATES && !found.contains_key(&url) {
            return;
        }
        let candidate = found
            .entry(url.clone())
            .or_insert_with(|| Candidate::new(url.clone()));
        let extension = extension(&url);
        let file = downloadable(extension.as_str());
        if file {
            candidate.add(ScoreReason::FileExtension);
        }
        if Builtin::ALL
            .iter()
            .any(|plugin| plugin.matches(&url) && !plugin.collection(&url))
        {
            candidate.add(ScoreReason::KnownHoster);
        }
        if label.split(|ch: char| !ch.is_alphanumeric()).any(|word| {
            ["download", "get", "scarica"]
                .iter()
                .any(|action| word.eq_ignore_ascii_case(action))
        }) {
            candidate.add(ScoreReason::DownloadLabel);
        }
        if attachment {
            candidate.add(ScoreReason::Attachment);
        }
        let stem = candidate
            .filename
            .rsplit_once('.')
            .map_or(candidate.filename.as_str(), |(stem, _)| stem);
        if stem.chars().count() >= 5 && normalize(&title).contains(&normalize(stem)) {
            candidate.add(ScoreReason::TitleMatch);
        }
        let host = url.host_str().unwrap_or_default();
        if blocked_host(host, blocked) {
            candidate.add(ScoreReason::Tracker);
        }
        if matches!(
            extension.as_str(),
            "css" | "js" | "ico" | "svg" | "woff" | "woff2"
        ) || ["sprite", "favicon", "/icons/"]
            .iter()
            .any(|marker| url.path().to_ascii_lowercase().contains(marker))
        {
            candidate.add(ScoreReason::InterfaceAsset);
        }
        if [
            "facebook.com",
            "instagram.com",
            "twitter.com",
            "x.com",
            "linkedin.com",
            "tiktok.com",
        ]
        .iter()
        .any(|domain| domain_matches(host, domain))
            || url.path_segments().is_some_and(|mut segments| {
                segments.any(|part| matches!(part, "login" | "signin" | "register" | "signup"))
            })
        {
            candidate.add(ScoreReason::SocialOrLogin);
        }
        candidate.classify(
            media || stream_url(&url) || candidate.group == CandidateGroup::Media,
            file,
        );
    };
    if let Ok(selector) = Selector::parse(
        "a[href], video[src], audio[src], source[src], iframe[src], meta[content], script",
    ) {
        for element in document.select(&selector) {
            match element.value().name() {
                "a" => {
                    if let Some(href) = element.attr("href").filter(|href| !href.starts_with('#')) {
                        let text = element.text().take(32).collect::<Vec<_>>().join(" ");
                        let label =
                            format!("{} {text}", element.attr("aria-label").unwrap_or_default());
                        add(href, &label, false, element.attr("download").is_some());
                    }
                }
                "meta" => {
                    let property = element.attr("property").unwrap_or_default();
                    if (property.starts_with("og:video") || property.starts_with("og:audio"))
                        && !property.ends_with(":width")
                        && !property.ends_with(":height")
                        && !property.ends_with(":type")
                        && let Some(value) = element.attr("content")
                    {
                        add(value, "", true, false);
                    }
                }
                "script" => {
                    let text = element.text().collect::<String>().replace("\\/", "/");
                    if element.attr("type") == Some("application/ld+json")
                        && let Ok(data) = serde_json::from_str::<serde_json::Value>(&text)
                    {
                        structured(&data, &mut add);
                    }
                    if let Some(regex) = SCRIPT_LINK.as_ref() {
                        for link in regex
                            .captures_iter(&text)
                            .filter_map(|capture| capture.get(1))
                            .take(MAX_CANDIDATES)
                        {
                            let value = link.as_str();
                            if base.join(value).ok().is_some_and(|url| {
                                downloadable(&extension(&url))
                                    || stream_url(&url)
                                    || Builtin::ALL.iter().any(|plugin| plugin.matches(&url))
                            }) {
                                add(value, "", false, false);
                            }
                        }
                    }
                }
                tag => {
                    if let Some(value) = element.attr("src") {
                        let media = tag != "iframe"
                            || ["embed", "player", "video"]
                                .iter()
                                .any(|part| value.contains(part));
                        add(value, "", media, false);
                    }
                }
            }
        }
    }
    for url in resources.iter().take(MAX_CANDIDATES) {
        add(url.as_str(), "", false, false);
    }
    let mut candidates = found.into_values().collect::<Vec<_>>();
    rank(&mut candidates);
    PageAnalysis::Page { title, candidates }
}

fn structured(data: &serde_json::Value, add: &mut impl FnMut(&str, &str, bool, bool)) {
    match data {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                if matches!(key.as_str(), "contentUrl" | "embedUrl" | "downloadUrl")
                    && let Some(url) = value.as_str()
                {
                    add(url, "", key != "downloadUrl", false);
                } else {
                    structured(value, add);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                structured(item, add);
            }
        }
        _ => {}
    }
}

pub(crate) async fn probe(ctx: &Context, base: &Url, analysis: &mut PageAnalysis) {
    let PageAnalysis::Page { candidates, .. } = analysis else {
        return;
    };
    let targets = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate.url.origin() == base.origin()
                && !candidate.reasons.contains(&ScoreReason::KnownHoster)
                && !candidate.reasons.contains(&ScoreReason::InterfaceAsset)
                && !candidate.reasons.contains(&ScoreReason::SocialOrLogin)
                && !candidate.reasons.contains(&ScoreReason::Tracker)
                && candidate.group != CandidateGroup::Media
        })
        .take(MAX_PROBES)
        .map(|(index, candidate)| (index, candidate.url.clone()))
        .collect::<Vec<_>>();
    let mut results = stream::iter(targets.into_iter().map(|(index, url)| async move {
        let response = ctx
            .client()
            .head(url)
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .ok()?;
        if !response.status().is_success() || http::is_html(&response) {
            return None;
        }
        let size = response
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok()?.parse::<u64>().ok());
        Some((index, size, http::attachment(&response)))
    }))
    .buffer_unordered(4);
    while let Some(result) = results.next().await {
        if let Some((index, size, attachment)) = result {
            let candidate = &mut candidates[index];
            candidate.size(size);
            if attachment {
                candidate.add(ScoreReason::Attachment);
            }
            candidate.classify(false, true);
        }
    }
    drop(results);
    rank(candidates);
}

pub(crate) fn read_hosts(value: &str) -> HashSet<String> {
    value
        .lines()
        .filter_map(|line| {
            let line = line.split('#').next()?.trim();
            let mut fields = line.split_ascii_whitespace();
            let first = fields.next()?;
            let domain = if first.parse::<std::net::IpAddr>().is_ok() {
                fields.next()?
            } else {
                first
                    .strip_prefix("||")
                    .and_then(|value| value.strip_suffix('^'))
                    .unwrap_or(first)
            };
            domain.contains('.').then(|| domain.to_ascii_lowercase())
        })
        .collect()
}

fn blocked_host(host: &str, list: &HashSet<String>) -> bool {
    [
        "doubleclick.net",
        "googlesyndication.com",
        "google-analytics.com",
        "adservice.google.com",
    ]
    .iter()
    .any(|domain| domain_matches(host, domain))
        || std::iter::once(host)
            .chain(host.match_indices('.').map(|(index, _)| &host[index + 1..]))
            .any(|part| list.contains(part))
}

fn domain_matches(host: &str, domain: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

fn rank(candidates: &mut [Candidate]) {
    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.url.as_str().cmp(right.url.as_str()))
    });
}

pub(crate) fn filename(url: &Url) -> String {
    let name = url
        .path_segments()
        .and_then(|segments| segments.rev().find(|seg| !seg.is_empty()))
        .unwrap_or_else(|| url.host_str().unwrap_or_default());
    percent_encoding::percent_decode_str(name)
        .decode_utf8_lossy()
        .chars()
        .take(255)
        .collect()
}

fn extension(url: &Url) -> String {
    filename(url)
        .rsplit_once('.')
        .map_or_else(String::new, |(_, ext)| ext.to_ascii_lowercase())
}

fn downloadable(extension: &str) -> bool {
    matches!(
        extension,
        "zip"
            | "7z"
            | "rar"
            | "tar"
            | "gz"
            | "xz"
            | "zst"
            | "iso"
            | "img"
            | "bin"
            | "exe"
            | "msi"
            | "dmg"
            | "deb"
            | "apk"
            | "pdf"
            | "epub"
            | "docx"
            | "xlsx"
            | "pptx"
            | "txt"
            | "csv"
            | "mp4"
            | "mkv"
            | "avi"
            | "mov"
            | "webm"
            | "mp3"
            | "flac"
            | "ogg"
            | "wav"
            | "torrent"
            | "dlc"
            | "metalink"
            | "m3u"
            | "png"
            | "jpg"
            | "jpeg"
            | "webp"
    )
}

fn stream_url(url: &Url) -> bool {
    matches!(extension(url).as_str(), "m3u8" | "mpd")
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
