//! Resolves public download links and inspects pages without executing their scripts.

mod accounts;
mod browser;
mod containers;
mod grabber;
mod http;
mod plugins;
mod recipes;

pub use accounts::{HosterAccount, HosterCredentials, KeyringAccounts};
pub use containers::{
    ContainerFile, DlcKeyService, ImportedLink, ImportedPackage, import_file, links,
};
pub use grabber::{Candidate, CandidateGroup, PageAnalysis, ScoreReason, analyze_html};
pub use plugins::{Builtin, IndexCrawler};
pub use recipes::{Recipe, RecipeStep};
pub use ruscello_browser::{
    BrowserOptions,
    import::Browser as InstalledBrowser,
    protocol::{Failure as BrowserFailure, Locale as BrowserLocale, Session as BrowserSession},
};

use std::{
    fmt,
    future::Future,
    num::NonZeroUsize,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime},
};

use reqwest::{
    Method, RequestBuilder, StatusCode, Url,
    cookie::CookieStore,
    header::{self, HeaderMap},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("invalid HTTP link")]
    InvalidLink,
    #[error("the host returned HTTP {0}")]
    Status(StatusCode),
    #[error("the host request failed")]
    Request(#[source] reqwest::Error),
    #[error("the host requires browser verification")]
    VerificationRequired,
    #[error(transparent)]
    Browser(BrowserFailure),
    #[error("the page does not expose a supported download")]
    NoDownload,
    #[error("the page or container exceeds the size limit")]
    TooLarge,
    #[error("the import file could not be read")]
    Read(#[from] std::io::Error),
    #[error("the download container is invalid")]
    InvalidContainer,
    #[error("the DLC key service is unavailable")]
    KeyService,
    #[error("the recipe is invalid")]
    InvalidRecipe,
    #[error("a recipe file is invalid: {0}")]
    RecipeFile(PathBuf),
    #[error("the hoster account details are invalid")]
    InvalidAccount,
    #[error("the operating system credential store is unavailable")]
    Keyring,
    #[error("the host requires an account token")]
    AccountRequired,
    #[error("link resolution was cancelled")]
    Cancelled,
}

impl From<reqwest::Error> for ResolveError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error.without_url())
    }
}

impl From<BrowserFailure> for ResolveError {
    fn from(error: BrowserFailure) -> Self {
        match error {
            BrowserFailure::VerificationRequired => Self::VerificationRequired,
            BrowserFailure::Cancelled => Self::Cancelled,
            BrowserFailure::NoDownload => Self::NoDownload,
            error => Self::Browser(error),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HosterLimits {
    pub connections: Option<NonZeroUsize>,
    pub parallel_downloads: Option<NonZeroUsize>,
}

impl HosterLimits {
    pub const SERIAL: Self = Self {
        connections: Some(NonZeroUsize::MIN),
        parallel_downloads: Some(NonZeroUsize::MIN),
    };
}

#[derive(Clone)]
pub struct Resolution {
    pub url: Url,
    pub headers: HeaderMap,
    pub limits: HosterLimits,
    pub expires: Option<SystemTime>,
    pub filename: Option<String>,
}

impl fmt::Debug for Resolution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Resolution")
            .field("limits", &self.limits)
            .field("expires", &self.expires)
            .finish_non_exhaustive()
    }
}

pub type ResolveFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ResolveError>> + Send + 'a>>;

pub trait HostPlugin: fmt::Debug + Send + Sync {
    fn name(&self) -> &str;
    fn matches(&self, url: &Url) -> bool;
    fn requires_browser(&self) -> bool {
        false
    }
    fn limits(&self) -> HosterLimits {
        HosterLimits::default()
    }
    fn resolve<'a>(&'a self, ctx: &'a Context, url: &'a Url) -> ResolveFuture<'a, Resolution>;
}

pub trait CrawlerPlugin: fmt::Debug + Send + Sync {
    fn matches(&self, url: &Url) -> bool;
    fn crawl<'a>(&'a self, ctx: &'a Context, url: &'a Url) -> ResolveFuture<'a, PageAnalysis>;
}

pub trait AccountStore: fmt::Debug + Send + Sync {
    fn get(&self, host: String) -> ResolveFuture<'_, Option<HosterCredentials>>;
}

/// Implemented by a browser helper capable of presenting verification to the user.
pub trait Browser: fmt::Debug + Send + Sync {
    fn resolve<'a>(
        &'a self,
        url: &'a Url,
        recipe: Option<&'a Recipe>,
        cancellation: &'a CancellationToken,
    ) -> ResolveFuture<'a, Resolution>;

    fn inspect<'a>(
        &'a self,
        _url: &'a Url,
        _cancellation: &'a CancellationToken,
    ) -> ResolveFuture<'a, BrowserPage> {
        Box::pin(std::future::ready(Err(ResolveError::VerificationRequired)))
    }

    fn verify<'a>(
        &'a self,
        _url: &'a Url,
        _locale: BrowserLocale,
        _cancellation: &'a CancellationToken,
    ) -> ResolveFuture<'a, BrowserSession> {
        Box::pin(std::future::ready(Err(BrowserFailure::Unavailable.into())))
    }

    fn read_session<'a>(
        &'a self,
        _url: &'a Url,
        _cancellation: &'a CancellationToken,
    ) -> ResolveFuture<'a, BrowserSession> {
        Box::pin(std::future::ready(Ok(BrowserSession::default())))
    }
}

pub struct BrowserPage {
    pub url: Url,
    pub html: String,
    pub resources: Vec<Url>,
    pub session: BrowserSession,
}

impl fmt::Debug for BrowserPage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserPage").finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub struct Resolver {
    client: reqwest::Client,
    cookies: Arc<reqwest::cookie::Jar>,
    plugins: Vec<Arc<dyn HostPlugin>>,
    crawlers: Vec<Arc<dyn CrawlerPlugin>>,
    recipes: Option<PathBuf>,
    browser: Option<Arc<dyn Browser>>,
    native_browser: Option<browser::NativeBrowser>,
    sessions: browser::Sessions,
    accounts: Arc<dyn AccountStore>,
}

impl Resolver {
    pub fn new(recipes: Option<PathBuf>) -> Result<Self, ResolveError> {
        let cookies = Arc::new(reqwest::cookie::Jar::default());
        let client = http::client().cookie_provider(cookies.clone()).build()?;
        Ok(Self {
            client,
            cookies,
            plugins: Builtin::ALL
                .into_iter()
                .map(|plugin| Arc::new(plugin) as Arc<dyn HostPlugin>)
                .collect(),
            crawlers: vec![Arc::new(IndexCrawler)],
            recipes,
            browser: None,
            native_browser: None,
            sessions: browser::Sessions::default(),
            accounts: Arc::new(KeyringAccounts),
        })
    }

    pub fn with_plugin(mut self, plugin: impl HostPlugin + 'static) -> Self {
        self.plugins.insert(0, Arc::new(plugin));
        self
    }

    pub fn with_browser(mut self, browser: impl Browser + 'static) -> Self {
        self.browser = Some(Arc::new(browser));
        self
    }

    pub fn with_browser_helper(mut self, options: BrowserOptions, enabled: bool) -> Self {
        let browser = browser::NativeBrowser::new(options, enabled);
        self.native_browser = Some(browser.clone());
        self.with_browser(browser)
    }

    pub fn enable_browser_helper(&self, enabled: bool) {
        if let Some(browser) = &self.native_browser {
            browser.enable(enabled);
        }
        if !enabled {
            self.sessions.clear();
        }
    }

    pub async fn stop_browser_helper(&self) {
        if let Some(browser) = &self.native_browser {
            browser.stop().await;
        }
    }

    pub async fn verify(
        &self,
        url: &Url,
        locale: BrowserLocale,
        cancellation: &CancellationToken,
    ) -> Result<(), ResolveError> {
        http::validate(url)?;
        let browser = self.browser.as_ref().ok_or(BrowserFailure::Unavailable)?;
        let session = browser.verify(url, locale, cancellation).await?;
        if session.user_agent.is_empty() {
            return Err(BrowserFailure::Unavailable.into());
        }
        self.sessions.store(url, session, true);
        Ok(())
    }

    pub async fn import_browser_cookies(
        &self,
        browser: InstalledBrowser,
        url: &Url,
    ) -> Result<usize, ResolveError> {
        http::validate(url)?;
        let helper = self
            .native_browser
            .as_ref()
            .ok_or(BrowserFailure::Unavailable)?;
        let (count, session) = helper.import(browser, url).await?;
        self.sessions.store(url, session, count > 0);
        Ok(count)
    }

    pub fn session_version(&self, url: &Url) -> u64 {
        self.sessions.version(url)
    }

    pub fn session_headers(&self, url: &Url) -> reqwest::header::HeaderMap {
        self.sessions.headers(url)
    }

    pub fn with_crawler(mut self, crawler: impl CrawlerPlugin + 'static) -> Self {
        self.crawlers.insert(0, Arc::new(crawler));
        self
    }

    pub fn with_accounts(mut self, accounts: impl AccountStore + 'static) -> Self {
        self.accounts = Arc::new(accounts);
        self
    }

    pub fn builtin(&self, url: &Url) -> Option<&dyn HostPlugin> {
        self.plugins
            .iter()
            .find(|plugin| plugin.matches(url))
            .map(AsRef::as_ref)
    }

    pub async fn limits(&self, url: &Url) -> Result<HosterLimits, ResolveError> {
        let recipes = recipes::load(self.recipes.as_deref()).await?;
        Ok(find_plugin(&recipes, self, url).map_or_else(HosterLimits::default, HostPlugin::limits))
    }

    /// A direct URL remains the HTTP engine's source. Only known hosters and recipes require an extra request.
    pub async fn resolve(
        &self,
        url: &Url,
        cancellation: &CancellationToken,
    ) -> Result<Option<Resolution>, ResolveError> {
        self.resolve_with_xfs(url, cancellation, false).await
    }

    pub async fn resolve_with_xfs(
        &self,
        url: &Url,
        cancellation: &CancellationToken,
        allow_generic_xfs: bool,
    ) -> Result<Option<Resolution>, ResolveError> {
        http::validate(url)?;
        let ctx = self.context(cancellation);
        tokio::select! {
            () = cancellation.cancelled() => Err(ResolveError::Cancelled),
            result = self.resolve_inner(url, &ctx, allow_generic_xfs) => result,
        }
    }

    /// Resolves again after HTTP rejects an existing transfer representation.
    pub async fn recover_link(
        &self,
        url: &Url,
        cancellation: &CancellationToken,
        allow_generic_xfs: bool,
        prefer_browser: bool,
    ) -> Result<Option<Resolution>, ResolveError> {
        http::validate(url)?;
        let ctx = self.context(cancellation);
        let recipes = recipes::load(self.recipes.as_deref()).await?;
        let fallback = allow_generic_xfs.then_some(&Builtin::XFileSharing as &dyn HostPlugin);
        let recover = async {
            if let Some(plugin) = find_plugin(&recipes, self, url).or(fallback) {
                let recipe = recipes.iter().find(|recipe| recipe.matches(url));
                if prefer_browser
                    && self.browser.is_some()
                    && !plugin.requires_browser()
                    && self.session_version(url) != 0
                {
                    let xfs =
                        (plugin.name() == Builtin::XFileSharing.name()).then(Recipe::xfs_browser);
                    let fallback = self
                        .browser_fallback(url, recipe.or(xfs.as_ref()), &ctx, plugin.limits())
                        .await;
                    if !matches!(fallback, Err(ResolveError::NoDownload)) {
                        return fallback.map(Some);
                    }
                }
                return self.resolve_inner(url, &ctx, allow_generic_xfs).await;
            }
            let response = ctx
                .get(url.clone())
                .header(header::RANGE, "bytes=0-")
                .send()
                .await?;
            let result = if http::is_html(&response) {
                http::page(response).await.map(|_| None)
            } else {
                http::status(&response).map(|()| {
                    Some(ctx.direct(response.url().clone(), url, HosterLimits::default()))
                })
            };
            if matches!(result, Err(ResolveError::VerificationRequired)) {
                return self
                    .browser_fallback(url, None, &ctx, HosterLimits::default())
                    .await
                    .map(Some);
            }
            result
        };
        tokio::select! {
            () = cancellation.cancelled() => Err(ResolveError::Cancelled),
            result = recover => result,
        }
    }

    async fn resolve_inner(
        &self,
        url: &Url,
        ctx: &Context,
        allow_generic_xfs: bool,
    ) -> Result<Option<Resolution>, ResolveError> {
        let recipes = recipes::load(self.recipes.as_deref()).await?;
        let recipe = recipes.iter().find(|recipe| recipe.matches(url));
        let fallback = allow_generic_xfs.then_some(&Builtin::XFileSharing as &dyn HostPlugin);
        let Some(plugin) = find_plugin(&recipes, self, url).or(fallback) else {
            let headers = self.sessions.headers(url);
            return Ok((!headers.is_empty())
                .then(|| http::direct(url.clone(), url, HosterLimits::default(), headers)));
        };
        let result = if plugin.requires_browser() {
            Err(ResolveError::VerificationRequired)
        } else {
            plugin.resolve(ctx, url).await
        };
        let resolution = match (result, &self.browser) {
            (Err(ResolveError::VerificationRequired), Some(_)) => {
                let xfs = (plugin.name() == Builtin::XFileSharing.name()).then(Recipe::xfs_browser);
                self.browser_fallback(url, recipe.or(xfs.as_ref()), ctx, plugin.limits())
                    .await?
            }
            (result, _) => result?,
        };
        http::validate(&resolution.url)?;
        tracing::debug!(plugin = plugin.name(), "Resolved hoster link");
        Ok(Some(resolution))
    }

    async fn browser_fallback(
        &self,
        url: &Url,
        recipe: Option<&Recipe>,
        ctx: &Context,
        limits: HosterLimits,
    ) -> Result<Resolution, ResolveError> {
        let browser = self
            .browser
            .as_ref()
            .ok_or(ResolveError::VerificationRequired)?;
        let mut resolution = browser
            .resolve(url, recipe, &ctx.cancellation)
            .await
            .map_err(|error| {
                if matches!(
                    error,
                    ResolveError::Browser(
                        BrowserFailure::Unavailable
                            | BrowserFailure::ProfileInUse
                            | BrowserFailure::Timeout
                    )
                ) {
                    ResolveError::VerificationRequired
                } else {
                    error
                }
            })?;
        resolution.limits = limits;
        Ok(resolution)
    }

    pub async fn inspect(
        &self,
        url: &Url,
        cancellation: &CancellationToken,
    ) -> Result<PageAnalysis, ResolveError> {
        http::validate(url)?;
        let ctx = self.context(cancellation);
        tokio::select! {
            () = cancellation.cancelled() => Err(ResolveError::Cancelled),
            result = self.inspect_inner(url, &ctx) => result,
        }
    }

    pub async fn inspect_browser(
        &self,
        url: &Url,
        cancellation: &CancellationToken,
    ) -> Result<PageAnalysis, ResolveError> {
        http::validate(url)?;
        let browser = self.browser.as_ref().ok_or(BrowserFailure::Unavailable)?;
        let page = browser.inspect(url, cancellation).await?;
        http::validate(&page.url)?;
        if page.html.len() > http::PAGE_LIMIT {
            return Err(ResolveError::TooLarge);
        }
        if page.html.is_empty() && page.resources.is_empty() {
            return Ok(PageAnalysis::direct(page.url, None));
        }
        self.sessions.store(&page.url, page.session, false);
        let ctx = self.context(cancellation);
        let mut analysis = ctx
            .analyze_page(&page.url, &page.html, &page.resources)
            .await;
        grabber::probe(&ctx, &page.url, &mut analysis).await;
        Ok(analysis)
    }

    async fn inspect_inner(&self, url: &Url, ctx: &Context) -> Result<PageAnalysis, ResolveError> {
        let recipes = recipes::load(self.recipes.as_deref()).await?;
        if let Some(plugin) = find_plugin(&recipes, self, url)
            && !Builtin::ALL
                .iter()
                .any(|builtin| builtin.name() == plugin.name() && builtin.collection(url))
        {
            if plugin.name() == Builtin::Gofile.name()
                && !url.query_pairs().any(|(key, _)| key == "file")
            {
                return plugins::inspect_gofile(ctx, url).await;
            }
            return Ok(PageAnalysis::hoster(url.clone(), plugin.name()));
        }
        let crawler = self
            .crawlers
            .iter()
            .find(|crawler| crawler.matches(url))
            .ok_or(ResolveError::NoDownload)?;
        match (crawler.crawl(ctx, url).await, &self.browser) {
            (Err(ResolveError::VerificationRequired), Some(_)) => self
                .inspect_browser(url, &ctx.cancellation)
                .await
                .map_err(|error| {
                    if matches!(
                        error,
                        ResolveError::Browser(
                            BrowserFailure::Unavailable | BrowserFailure::ProfileInUse
                        )
                    ) {
                        ResolveError::VerificationRequired
                    } else {
                        error
                    }
                }),
            (result, _) => result,
        }
    }

    fn context(&self, cancellation: &CancellationToken) -> Context {
        Context {
            resolver: self.clone(),
            cancellation: cancellation.clone(),
        }
    }
}

fn find_plugin<'a>(
    recipes: &'a [Recipe],
    resolver: &'a Resolver,
    url: &Url,
) -> Option<&'a dyn HostPlugin> {
    recipes
        .iter()
        .find(|recipe| recipe.matches(url))
        .map(|recipe| recipe as &dyn HostPlugin)
        .or_else(|| resolver.builtin(url))
}

#[derive(Debug, Clone)]
pub struct Context {
    resolver: Resolver,
    pub cancellation: CancellationToken,
}

impl Context {
    pub fn client(&self) -> &reqwest::Client {
        &self.resolver.client
    }

    fn request(&self, method: Method, url: Url) -> RequestBuilder {
        let headers = self.resolver.sessions.headers(&url);
        self.client().request(method, url).headers(headers)
    }

    pub fn get(&self, url: Url) -> RequestBuilder {
        self.request(Method::GET, url)
    }

    pub fn post(&self, url: Url) -> RequestBuilder {
        self.request(Method::POST, url)
    }

    pub fn head(&self, url: Url) -> RequestBuilder {
        self.request(Method::HEAD, url)
    }

    pub async fn account(&self, host: &str) -> Result<Option<HosterCredentials>, ResolveError> {
        self.resolver.accounts.get(host.to_owned()).await
    }

    pub async fn analyze_page(&self, base: &Url, body: &str, resources: &[Url]) -> PageAnalysis {
        use tokio::io::AsyncReadExt as _;
        let mut sources = String::new();
        if let Some(directory) = &self.resolver.recipes
            && let Ok(file) = tokio::fs::File::open(directory.join("blocked-hosts.txt")).await
        {
            let _ = file.take(1024 * 1024).read_to_string(&mut sources).await;
        }
        grabber::analyze_with_filter(base, body, resources, &grabber::read_hosts(&sources))
    }

    pub async fn wait(&self, duration: Duration) -> Result<(), ResolveError> {
        tokio::select! {
            () = self.cancellation.cancelled() => Err(ResolveError::Cancelled),
            () = tokio::time::sleep(duration) => Ok(()),
        }
    }

    pub fn direct(&self, url: Url, referer: &Url, limits: HosterLimits) -> Resolution {
        let mut headers = self.resolver.sessions.headers(&url);
        if !headers.contains_key(header::COOKIE)
            && let Some(mut cookie) = self.resolver.cookies.cookies(&url)
        {
            cookie.set_sensitive(true);
            headers.insert(header::COOKIE, cookie);
        }
        http::direct(url, referer, limits, headers)
    }
}
