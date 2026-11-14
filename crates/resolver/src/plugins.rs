use std::{
    sync::LazyLock,
    time::{Duration, SystemTime},
};

use regex::Regex;
use reqwest::{
    StatusCode, Url,
    header::{self, HeaderValue},
};
use scraper::Html;

use crate::{
    Context, CrawlerPlugin, HostPlugin, HosterLimits, PageAnalysis, Resolution, ResolveError,
    ResolveFuture, grabber, http,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    FuckingFast,
    XFileSharing,
    Pixeldrain,
    Gofile,
    MediaFire,
    GoogleDrive,
    Dropbox,
    OneDrive,
    GitHubReleases,
    SourceForge,
    InternetArchive,
}

impl Builtin {
    pub const ALL: [Self; 11] = [
        Self::FuckingFast,
        Self::XFileSharing,
        Self::Pixeldrain,
        Self::Gofile,
        Self::MediaFire,
        Self::GoogleDrive,
        Self::Dropbox,
        Self::OneDrive,
        Self::GitHubReleases,
        Self::SourceForge,
        Self::InternetArchive,
    ];

    pub(crate) fn collection(self, url: &Url) -> bool {
        match self {
            Self::GitHubReleases => !url.path().contains("/releases/download/"),
            Self::InternetArchive => url.path().starts_with("/details/"),
            _ => false,
        }
    }

    async fn run(self, ctx: &Context, url: &Url) -> Result<Resolution, ResolveError> {
        let result = match self {
            Self::FuckingFast => {
                if url.path().starts_with("/dl/") {
                    url.clone()
                } else {
                    fuckingfast(ctx, url).await?
                }
            }
            Self::XFileSharing => xfilesharing(ctx, url).await?,
            Self::Pixeldrain => pixel_url(url)?,
            Self::Gofile => return gofile(ctx, url).await,
            Self::MediaFire => {
                let response = ctx.get(url.clone()).send().await?;
                let base = response.url().clone();
                let body = http::page(response).await?;
                selected_link(&base, &body, "#downloadButton[href], a.input[href]")
                    .ok_or(ResolveError::NoDownload)?
            }
            Self::GoogleDrive => drive(ctx, url).await?,
            Self::Dropbox => with_query(url, "dl", "1"),
            Self::OneDrive => onedrive(ctx, url).await?,
            Self::GitHubReleases | Self::InternetArchive if self.collection(url) => {
                return Err(ResolveError::NoDownload);
            }
            Self::GitHubReleases | Self::InternetArchive => url.clone(),
            Self::SourceForge => {
                let path = url
                    .path()
                    .strip_prefix("/projects/")
                    .ok_or(ResolveError::InvalidLink)?;
                let (project, file) = path.split_once("/files/").ok_or(ResolveError::NoDownload)?;
                let file = file
                    .strip_suffix("/download")
                    .ok_or(ResolveError::NoDownload)?;
                Url::parse(&format!(
                    "https://downloads.sourceforge.net/project/{project}/{file}"
                ))
                .map_err(|_| ResolveError::InvalidLink)?
            }
        };
        let mut resolution = ctx.direct(result, url, self.limits());
        if self == Self::FuckingFast && !url.path().starts_with("/dl/") {
            resolution.expires = SystemTime::now().checked_add(Duration::from_secs(23 * 3600));
        }
        if self == Self::Pixeldrain
            && let Ok(Some(account)) = ctx.account("pixeldrain.com").await
        {
            let mut auth = HeaderValue::from_str(&format!("Bearer {}", account.password))
                .map_err(|_| ResolveError::InvalidAccount)?;
            auth.set_sensitive(true);
            resolution.headers.insert(header::AUTHORIZATION, auth);
        }
        Ok(resolution)
    }
}

impl HostPlugin for Builtin {
    fn name(&self) -> &str {
        match self {
            Self::FuckingFast => "fuckingfast",
            Self::XFileSharing => "XFileSharing",
            Self::Pixeldrain => "Pixeldrain",
            Self::Gofile => "Gofile",
            Self::MediaFire => "MediaFire",
            Self::GoogleDrive => "Google Drive",
            Self::Dropbox => "Dropbox",
            Self::OneDrive => "OneDrive",
            Self::GitHubReleases => "GitHub Releases",
            Self::SourceForge => "SourceForge",
            Self::InternetArchive => "Internet Archive",
        }
    }

    fn matches(&self, url: &Url) -> bool {
        let host = url
            .host_str()
            .unwrap_or_default()
            .trim_start_matches("www.");
        let path = url.path();
        match self {
            Self::FuckingFast => host == "fuckingfast.co" && path != "/",
            Self::XFileSharing => host == "datanodes.to" && path != "/",
            Self::Pixeldrain => {
                matches!(host, "pixeldrain.com" | "pixeldrain.net") && path.starts_with("/u/")
            }
            Self::Gofile => host == "gofile.io" && path.starts_with("/d/"),
            Self::MediaFire => host == "mediafire.com" && path.starts_with("/file/"),
            Self::GoogleDrive => {
                host == "drive.google.com"
                    && (path.starts_with("/file/d/") || matches!(path, "/open" | "/uc"))
            }
            Self::Dropbox => {
                host == "dropbox.com" && (path.starts_with("/s/") || path.starts_with("/scl/fi/"))
            }
            Self::OneDrive => {
                matches!(host, "1drv.ms" | "onedrive.live.com")
                    && (path != "/" || url.query().is_some())
            }
            Self::GitHubReleases => {
                host == "github.com" && (path.ends_with("/releases") || path.contains("/releases/"))
            }
            Self::SourceForge => {
                host == "sourceforge.net"
                    && path.starts_with("/projects/")
                    && path.ends_with("/download")
            }
            Self::InternetArchive => {
                host == "archive.org"
                    && (path.starts_with("/details/") || path.starts_with("/download/"))
            }
        }
    }

    fn limits(&self) -> HosterLimits {
        match self {
            Self::FuckingFast | Self::XFileSharing | Self::Gofile => HosterLimits::SERIAL,
            _ => HosterLimits::default(),
        }
    }

    fn resolve<'a>(&'a self, ctx: &'a Context, url: &'a Url) -> ResolveFuture<'a, Resolution> {
        Box::pin(self.run(ctx, url))
    }
}

fn selected_link(base: &Url, body: &str, selector: &str) -> Option<Url> {
    http::join(
        base,
        http::first(&Html::parse_document(body), selector)?.attr("href")?,
    )
}

static OPEN: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"window\s*\.\s*open\s*\(\s*["']([^"']+)["']"#).ok());

async fn fuckingfast(ctx: &Context, source: &Url) -> Result<Url, ResolveError> {
    let response = ctx.get(source.clone()).send().await?;
    let base = response.url().clone();
    let direct = |value: &str| {
        http::join(&base, value)
            .filter(|url| url.origin() == source.origin() && url.path().starts_with("/dl/"))
    };
    if let Err(error) = http::status(&response) {
        if http::is_html(&response) {
            http::page(response).await?;
        }
        return Err(error);
    }
    if base.origin() == source.origin() && base.path().starts_with("/dl/") {
        return Ok(base);
    }
    if let Some(url) = response
        .headers()
        .get("hx-redirect")
        .and_then(|value| value.to_str().ok())
        .and_then(direct)
    {
        return Ok(url);
    }
    let body = http::page(response).await?;
    fuckingfast_link(source, &base, &body).ok_or(ResolveError::NoDownload)
}

pub(crate) fn fuckingfast_link(source: &Url, base: &Url, body: &str) -> Option<Url> {
    OPEN.as_ref()?.captures_iter(body).find_map(|capture| {
        http::join(base, capture.get(1)?.as_str())
            .filter(|url| url.origin() == source.origin() && url.path().starts_with("/dl/"))
    })
}

pub(crate) fn browser_direct_link(base: &Url, body: &str) -> Option<Url> {
    if Builtin::MediaFire.matches(base) {
        selected_link(base, body, "#downloadButton[href], a.input[href]")
    } else {
        selected_link(base, body, "a[download][href]")
    }
}

async fn xfilesharing(ctx: &Context, source: &Url) -> Result<Url, ResolveError> {
    let mut response = ctx.get(source.clone()).send().await?;
    for _ in 0..4 {
        let base = response.url().clone();
        if !http::is_html(&response) {
            http::status(&response)?;
            return Ok(base);
        }
        let body = http::page(response).await?;
        let (form, delay) = xfs_step(&base, &body)?;
        match form {
            XfsStep::Link(url) => {
                ctx.wait(delay).await?;
                return Ok(url);
            }
            XfsStep::Form(form) => {
                ctx.wait(delay).await?;
                response = form.send(ctx, &base).await?;
            }
        }
    }
    Err(ResolveError::NoDownload)
}

enum XfsStep {
    Link(Url),
    Form(http::Form),
}

fn xfs_step(base: &Url, body: &str) -> Result<(XfsStep, Duration), ResolveError> {
    let document = Html::parse_document(body);
    let countdown = http::first(
        &document,
        "#countdown [data-seconds], #countdown span, #countdown, #seconds, [data-countdown]",
    );
    let delay = countdown
        .map(|element| {
            let value = element
                .attr("data-seconds")
                .or_else(|| element.attr("data-countdown"))
                .map_or_else(|| element.text().collect::<String>(), str::to_owned);
            value
                .split(|ch: char| !ch.is_ascii_digit())
                .find(|part| !part.is_empty())
                .and_then(|part| part.parse::<u64>().ok())
                .filter(|seconds| *seconds <= 3600)
                .map(Duration::from_secs)
                .ok_or(ResolveError::NoDownload)
        })
        .transpose()?
        .unwrap_or_default();
    if let Some(url) = http::first(
        &document,
        "a#downloadbtn[href], a#downloadlink[href], a#direct_link[href]",
    )
    .and_then(|element| http::join(base, element.attr("href")?))
    {
        return Ok((XfsStep::Link(url), delay));
    }
    for (operation, button) in [
        ("download2", "#downloadbtn"),
        ("download1", "#method_free, [name=method_free]"),
    ] {
        let selector = format!("form:has(input[name=op][value={operation}])");
        if let Some(form) = http::first(&document, &selector) {
            let query = scraper::Selector::parse(button).map_err(|_| ResolveError::NoDownload)?;
            let clicked = form.select(&query).next().map(|_| button);
            return Ok((
                XfsStep::Form(http::Form::read(&document, base, &selector, clicked)?),
                delay,
            ));
        }
    }
    Err(ResolveError::NoDownload)
}

fn pixel_url(url: &Url) -> Result<Url, ResolveError> {
    let id = url
        .path()
        .strip_prefix("/u/")
        .and_then(|path| path.split('/').next())
        .filter(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        .ok_or(ResolveError::InvalidLink)?;
    Url::parse(&format!("https://pixeldrain.com/api/file/{id}?download"))
        .map_err(|_| ResolveError::InvalidLink)
}

fn with_query(url: &Url, key: &str, value: &str) -> Url {
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(name, _)| name != key)
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    let mut url = url.clone();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained)
        .append_pair(key, value);
    url
}

async fn onedrive(ctx: &Context, source: &Url) -> Result<Url, ResolveError> {
    let direct = with_query(source, "download", "1");
    if source.host_str() != Some("1drv.ms") {
        return Ok(direct);
    }
    let response = ctx.get(direct).send().await?;
    let final_url = response.url().clone();
    if !http::is_html(&response) {
        http::status(&response)?;
        return Ok(final_url);
    }
    let body = http::page(response).await?;
    if final_url.host_str() != Some("onedrive.live.com") {
        return Err(ResolveError::NoDownload);
    }
    Ok(selected_link(&final_url, &body, "a[download][href]")
        .unwrap_or_else(|| with_query(&final_url, "download", "1")))
}

async fn drive(ctx: &Context, source: &Url) -> Result<Url, ResolveError> {
    let url = drive_url(source)?;
    let response = ctx.get(url).send().await?;
    let base = response.url().clone();
    http::status(&response)?;
    if !http::is_html(&response) {
        return Ok(base);
    }
    let body = http::page(response).await?;
    if !matches!(
        base.host_str(),
        Some("drive.usercontent.google.com" | "drive.google.com")
    ) {
        return Err(ResolveError::VerificationRequired);
    }
    let response = drive_form(&base, &body)?.send(ctx, &base).await?;
    http::status(&response)?;
    if http::is_html(&response) {
        return Err(ResolveError::NoDownload);
    }
    Ok(response.url().clone())
}

fn drive_form(base: &Url, body: &str) -> Result<http::Form, ResolveError> {
    http::Form::read(
        &Html::parse_document(body),
        base,
        "form#download-form",
        None,
    )
}

fn drive_url(source: &Url) -> Result<Url, ResolveError> {
    let id = source
        .path()
        .strip_prefix("/file/d/")
        .and_then(|path| path.split('/').next())
        .map(str::to_owned)
        .or_else(|| {
            source
                .query_pairs()
                .find(|(key, _)| key == "id")
                .map(|(_, value)| value.into_owned())
        })
        .filter(|id| !id.is_empty())
        .ok_or(ResolveError::NoDownload)?;
    let mut url = Url::parse("https://drive.usercontent.google.com/download")
        .map_err(|_| ResolveError::InvalidLink)?;
    url.query_pairs_mut()
        .append_pair("id", &id)
        .append_pair("export", "download");
    if let Some((_, key)) = source.query_pairs().find(|(name, _)| name == "resourcekey") {
        url.query_pairs_mut().append_pair("resourcekey", &key);
    }
    Ok(url)
}

async fn gofile(ctx: &Context, source: &Url) -> Result<Resolution, ResolveError> {
    let (account, body) = gofile_contents(ctx, source).await?;
    let mut result = gofile_link(ctx, source, &body)?;
    let mut cookie = HeaderValue::from_str(&format!("accountToken={}", account.password))
        .map_err(|_| ResolveError::InvalidAccount)?;
    cookie.set_sensitive(true);
    result.headers.insert(header::COOKIE, cookie);
    Ok(result)
}

async fn gofile_contents(
    ctx: &Context,
    source: &Url,
) -> Result<(crate::HosterCredentials, String), ResolveError> {
    let account = ctx
        .account("gofile.io")
        .await?
        .ok_or(ResolveError::AccountRequired)?;
    let id = source
        .path()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|id| !id.is_empty())
        .ok_or(ResolveError::InvalidLink)?;
    let mut api =
        Url::parse("https://api.gofile.io/contents/").map_err(|_| ResolveError::InvalidLink)?;
    api.path_segments_mut()
        .map_err(|_| ResolveError::InvalidLink)?
        .pop_if_empty()
        .push(id);
    let response = ctx.get(api).bearer_auth(&account.password).send().await?;
    if response.status() == StatusCode::UNAUTHORIZED {
        return Err(ResolveError::AccountRequired);
    }
    let body = http::body(response, http::PAGE_LIMIT).await?;
    Ok((account, body))
}

fn gofile_link(ctx: &Context, source: &Url, body: &str) -> Result<Resolution, ResolveError> {
    let data = gofile_data(body)?;
    let selected = source
        .query_pairs()
        .find(|(name, _)| name == "file")
        .map(|(_, value)| value);
    let child = data["children"].as_object().and_then(|children| {
        if let Some(selected) = selected {
            children.get(selected.as_ref())
        } else if children.len() == 1 {
            children.values().next()
        } else {
            None
        }
    });
    let file = if data["link"].is_string() {
        &data
    } else {
        child.ok_or(ResolveError::NoDownload)?
    };
    let target = file["link"]
        .as_str()
        .and_then(|value| http::join(source, value))
        .filter(|url| {
            url.scheme() == "https"
                && url
                    .host_str()
                    .is_some_and(|host| host.ends_with(".gofile.io"))
        })
        .ok_or(ResolveError::NoDownload)?;
    let mut result = ctx.direct(target, source, HosterLimits::SERIAL);
    result.filename = file["name"].as_str().map(str::to_owned);
    Ok(result)
}

fn gofile_data(body: &str) -> Result<serde_json::Value, ResolveError> {
    let mut value: serde_json::Value =
        serde_json::from_str(body).map_err(|_| ResolveError::NoDownload)?;
    match value["status"].as_str() {
        Some("ok") => Ok(value["data"].take()),
        Some("error-notPremium" | "error-token") => Err(ResolveError::AccountRequired),
        _ => Err(ResolveError::NoDownload),
    }
}

pub(crate) async fn inspect_gofile(
    ctx: &Context,
    source: &Url,
) -> Result<PageAnalysis, ResolveError> {
    let (_, body) = gofile_contents(ctx, source).await?;
    let data = gofile_data(&body)?;
    let Some(children) = data["children"]
        .as_object()
        .filter(|children| children.len() > 1)
    else {
        return Ok(PageAnalysis::hoster(source.clone(), Builtin::Gofile.name()));
    };
    let candidates = children
        .iter()
        .take(400)
        .filter(|(_, child)| child["link"].is_string())
        .map(|(id, child)| {
            grabber::hoster_candidate(
                with_query(source, "file", id),
                child["name"].as_str(),
                child["size"].as_u64(),
            )
        })
        .collect();
    let title = data["name"]
        .as_str()
        .unwrap_or(Builtin::Gofile.name())
        .to_owned();
    Ok(PageAnalysis::Page { title, candidates })
}

#[derive(Debug, Clone, Copy)]
pub struct IndexCrawler;

impl CrawlerPlugin for IndexCrawler {
    fn matches(&self, url: &Url) -> bool {
        http::validate(url).is_ok()
    }
    fn crawl<'a>(&'a self, ctx: &'a Context, url: &'a Url) -> ResolveFuture<'a, PageAnalysis> {
        Box::pin(async move {
            let response = ctx.get(url.clone()).send().await?;
            if response.status() == StatusCode::RANGE_NOT_SATISFIABLE {
                return Ok(PageAnalysis::direct(url.clone(), Some(0)));
            }
            if !http::is_html(&response) {
                http::status(&response)?;
                return Ok(PageAnalysis::from_response(url.clone(), &response));
            }
            let base = response.url().clone();
            let body = http::page(response).await?;
            if let Ok((XfsStep::Form(_), _)) = xfs_step(&base, &body) {
                return Ok(PageAnalysis::hoster(
                    url.clone(),
                    Builtin::XFileSharing.name(),
                ));
            }
            let mut analysis = ctx.analyze_page(&base, &body, &[]).await;
            grabber::probe(ctx, &base, &mut analysis).await;
            Ok(analysis)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HosterCredentials, Resolver};
    use ruscello_testkit::{FileSpec, Reply, TestServer};
    use std::sync::{Arc, Mutex};
    use tokio_util::sync::CancellationToken;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn parses_fuckingfast_html_and_refuses_verification() -> TestResult {
        let server = TestServer::start().await?;
        let url = Url::parse(&server.route("/share", |_| {
            Reply::html(include_str!(
                "../tests/fixtures/fuckingfast-2026-11-14.html"
            ))
        }))?;
        let blocked = Url::parse(&server.route("/challenge", |_| {
            Reply::html(include_str!("../tests/fixtures/challenge-2026-11-14.html"))
        }))?;
        let resolver = Resolver::new(None)?;
        let ctx = resolver.context(&CancellationToken::new());
        let result = Builtin::FuckingFast.resolve(&ctx, &url).await?;
        assert_eq!(result.url.path(), "/dl/fixture-token");
        assert_eq!(result.limits, HosterLimits::SERIAL);
        assert!(matches!(
            Builtin::FuckingFast.resolve(&ctx, &blocked).await,
            Err(ResolveError::VerificationRequired)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn xfs_submits_free_forms_after_countdown() -> TestResult {
        let server = TestServer::start().await?;
        let file = server.file(FileSpec::random(7, 42));
        let submitted = Arc::new(Mutex::new(None::<tokio::time::Instant>));
        let url = Url::parse(&server.route("/xfs", move |request| {
            let form = String::from_utf8_lossy(&request.body);
            if request.method == ruscello_testkit::Method::GET {
                Reply::html(include_str!("../tests/fixtures/xfs-free-2026-11-14.html"))
            } else if form.contains("op=download1") && form.contains("method_free=Free+Download") {
                *submitted
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(tokio::time::Instant::now());
                Reply::html(include_str!(
                    "../tests/fixtures/xfs-countdown-2026-11-14.html"
                ))
            } else if form.contains("op=download2")
                && form.contains("download=yes")
                && submitted
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_some_and(|time| time.elapsed() >= Duration::from_secs(1))
            {
                let mut headers = reqwest::header::HeaderMap::new();
                if let Ok(location) = HeaderValue::from_str(file.url()) {
                    headers.insert(header::LOCATION, location);
                }
                Reply::Response(StatusCode::SEE_OTHER, headers, String::new())
            } else {
                Reply::status(StatusCode::FORBIDDEN)
            }
        }))?;
        assert!(
            Builtin::XFileSharing
                .matches(&Url::parse("https://datanodes.to/abcdefghijkl/file.zip")?)
        );
        let ctx = Resolver::new(None)?.context(&CancellationToken::new());
        let result = Builtin::XFileSharing.resolve(&ctx, &url).await?;
        assert!(result.url.path().ends_with("payload.bin"));
        Ok(())
    }

    #[derive(Debug)]
    struct FixtureCredentials;

    impl crate::AccountStore for FixtureCredentials {
        fn get(&self, _host: String) -> ResolveFuture<'_, Option<HosterCredentials>> {
            Box::pin(std::future::ready(Ok(Some(HosterCredentials {
                username: String::new(),
                password: "fixture-token".into(),
            }))))
        }
    }

    #[tokio::test]
    async fn public_url_plugins_select_download_endpoints() -> TestResult {
        let ctx = Resolver::new(None)?
            .with_accounts(FixtureCredentials)
            .context(&CancellationToken::new());
        for (plugin, original, expected) in [
            (
                Builtin::Pixeldrain,
                "https://pixeldrain.com/u/Fixture1",
                "https://pixeldrain.com/api/file/Fixture1?download",
            ),
            (
                Builtin::Dropbox,
                "https://www.dropbox.com/scl/fi/demo/file.pdf?rlkey=demo&dl=0",
                "https://www.dropbox.com/scl/fi/demo/file.pdf?rlkey=demo&dl=1",
            ),
            (
                Builtin::OneDrive,
                "https://onedrive.live.com/?id=demo&download=0",
                "https://onedrive.live.com/?id=demo&download=1",
            ),
            (
                Builtin::GitHubReleases,
                "https://github.com/team/example/releases/download/v1/example.zip",
                "https://github.com/team/example/releases/download/v1/example.zip",
            ),
            (
                Builtin::SourceForge,
                "https://sourceforge.net/projects/demo/files/v1/demo.zip/download",
                "https://downloads.sourceforge.net/project/demo/v1/demo.zip",
            ),
            (
                Builtin::InternetArchive,
                "https://archive.org/download/demo/demo.iso",
                "https://archive.org/download/demo/demo.iso",
            ),
        ] {
            let original = Url::parse(original)?;
            assert!(plugin.matches(&original));
            let resolved = plugin.resolve(&ctx, &original).await?;
            assert_eq!(resolved.url.as_str(), expected);
            if plugin == Builtin::Pixeldrain {
                assert!(
                    resolved
                        .headers
                        .get(header::AUTHORIZATION)
                        .is_some_and(HeaderValue::is_sensitive)
                );
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn extracts_mediafire_gofile_and_drive_confirmation() -> TestResult {
        let server = TestServer::start().await?;
        let url = Url::parse(&server.route("/mediafire", |_| {
            Reply::html(include_str!("../tests/fixtures/mediafire-2026-11-14.html"))
        }))?;
        let ctx = Resolver::new(None)?.context(&CancellationToken::new());
        let result = Builtin::MediaFire.resolve(&ctx, &url).await?;
        assert_eq!(result.url.host_str(), Some("download123.mediafire.com"));
        let result = gofile_link(
            &ctx,
            &Url::parse("https://gofile.io/d/fixture")?,
            include_str!("../tests/fixtures/gofile-2026-11-14.json"),
        )?;
        assert_eq!(result.filename.as_deref(), Some("Example.zip"));
        assert_eq!(result.url.host_str(), Some("store1.gofile.io"));
        let drive =
            Url::parse("https://drive.google.com/file/d/fixture-id/view?resourcekey=fixture-key")?;
        assert_eq!(
            drive_url(&drive)?.as_str(),
            "https://drive.usercontent.google.com/download?id=fixture-id&export=download&resourcekey=fixture-key"
        );
        let url = Url::parse(&server.route("/download", |request| {
            if request.uri.query().is_some_and(|query| {
                query.contains("confirm=fixture-confirm") && query.contains("uuid=fixture-uuid")
            }) {
                Reply::status(StatusCode::NO_CONTENT)
            } else {
                Reply::status(StatusCode::BAD_REQUEST)
            }
        }))?;
        let form = drive_form(
            &url,
            include_str!("../tests/fixtures/drive-confirm-2026-11-14.html"),
        )?;
        assert_eq!(
            form.send(&ctx, &url).await?.status(),
            StatusCode::NO_CONTENT
        );
        Ok(())
    }

    #[tokio::test]
    async fn linkgrabber_crawls_release_assets_and_probes_direct_files() -> TestResult {
        let server = TestServer::start().await?;
        let file = server.file(FileSpec::random(7, 42).filename("test.zip"));
        let page = Url::parse(&server.route("/project", |_| {
            Reply::html(include_str!(
                "../tests/fixtures/collections-2026-11-14.html"
            ))
        }))?;
        let resolver = Resolver::new(None)?;
        let token = CancellationToken::new();
        assert!(
            matches!(resolver.inspect(&Url::parse(file.url())?, &token).await?, PageAnalysis::Direct(candidate) if candidate.size == Some(42))
        );
        assert!(
            matches!(resolver.inspect(&page, &token).await?, PageAnalysis::Page { candidates, .. } if candidates.len() == 4)
        );
        for (url, file) in [
            ("https://github.com/team/example/releases", "example-v1.zip"),
            ("https://archive.org/details/example", "example.iso"),
        ] {
            let base = Url::parse(url)?;
            assert!(
                matches!(grabber::analyze_html(&base, include_str!("../tests/fixtures/collections-2026-11-14.html"), &[]),
                PageAnalysis::Page { candidates, .. } if candidates.iter().any(|candidate| candidate.filename == file))
            );
        }
        Ok(())
    }
}
