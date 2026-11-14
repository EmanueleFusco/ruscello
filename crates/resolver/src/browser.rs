use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

use reqwest::{
    Url,
    cookie::{CookieStore as _, Jar},
    header::{self, HeaderValue},
};
use ruscello_browser::{
    BrowserClient, BrowserOptions,
    import::{self, Browser as InstalledBrowser},
    protocol::{Failure, Locale, Request, Response, Session, Step, View},
};
use tokio_util::sync::CancellationToken;

use crate::{
    Browser, BrowserPage, HostPlugin as _, Recipe, RecipeStep, Resolution, ResolveError,
    ResolveFuture, http,
};

#[derive(Debug, Clone)]
pub(crate) struct NativeBrowser {
    client: BrowserClient,
    enabled: Arc<AtomicBool>,
}

impl NativeBrowser {
    pub(crate) fn new(options: BrowserOptions, enabled: bool) -> Self {
        Self {
            client: BrowserClient::new(options),
            enabled: Arc::new(AtomicBool::new(enabled)),
        }
    }

    pub(crate) fn enable(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub(crate) async fn stop(&self) {
        self.client.stop().await;
    }

    async fn open(
        &self,
        url: &Url,
        view: View,
        steps: Vec<Step>,
        cancellation: &CancellationToken,
    ) -> Result<Response, ResolveError> {
        if !self.enabled.load(Ordering::Relaxed) {
            return Err(ResolveError::VerificationRequired);
        }
        Ok(self
            .client
            .request(
                Request::Open {
                    url: url.clone(),
                    view,
                    steps,
                },
                cancellation,
            )
            .await?)
    }

    pub(crate) async fn import(
        &self,
        browser: InstalledBrowser,
        url: &Url,
    ) -> Result<(usize, Session), ResolveError> {
        if !self.enabled.load(Ordering::Relaxed) {
            return Err(Failure::Unavailable.into());
        }
        let site = url.clone();
        let cookies = tokio::task::spawn_blocking(move || import::installed(browser, &site))
            .await
            .map_err(|_| Failure::Unavailable)?;
        if cookies.is_empty() {
            return Ok((0, Session::default()));
        }
        let cancellation = CancellationToken::new();
        let result = self
            .client
            .request(
                Request::Import {
                    url: url.clone(),
                    cookies,
                },
                &cancellation,
            )
            .await?;
        let Response::Imported { count } = result else {
            return Err(Failure::Unavailable.into());
        };
        Ok((count, self.read_session(url, &cancellation).await?))
    }

    async fn session(
        &self,
        url: &Url,
        view: View,
        cancellation: &CancellationToken,
    ) -> Result<Session, ResolveError> {
        match self.open(url, view, Vec::new(), cancellation).await? {
            Response::Session { session } => Ok(session),
            _ => Err(Failure::Unavailable.into()),
        }
    }
}

impl Browser for NativeBrowser {
    fn resolve<'a>(
        &'a self,
        url: &'a Url,
        recipe: Option<&'a Recipe>,
        cancellation: &'a CancellationToken,
    ) -> ResolveFuture<'a, Resolution> {
        Box::pin(async move {
            let steps =
                recipe.map_or_else(Vec::new, |recipe| recipe.steps.iter().map(step).collect());
            let limits = recipe.map_or_else(Default::default, |recipe| recipe.limits());
            match self.open(url, View::Resolve, steps, cancellation).await? {
                Response::Download {
                    url,
                    referer,
                    session,
                } => Ok(http::direct(
                    url,
                    &referer,
                    limits,
                    session_headers(&session),
                )),
                Response::Page {
                    url: page,
                    html,
                    session,
                    ..
                } => {
                    let direct = crate::Builtin::FuckingFast
                        .matches(url)
                        .then(|| crate::plugins::fuckingfast_link(url, &page, &html))
                        .flatten()
                        .or_else(|| crate::plugins::browser_direct_link(&page, &html))
                        .ok_or(ResolveError::NoDownload)?;
                    let session =
                        if direct.origin() == page.origin() && direct.path() == page.path() {
                            session
                        } else {
                            self.read_session(&direct, cancellation).await?
                        };
                    Ok(http::direct(
                        direct,
                        &page,
                        limits,
                        session_headers(&session),
                    ))
                }
                _ => Err(Failure::Unavailable.into()),
            }
        })
    }

    fn inspect<'a>(
        &'a self,
        url: &'a Url,
        cancellation: &'a CancellationToken,
    ) -> ResolveFuture<'a, BrowserPage> {
        Box::pin(async move {
            match self
                .open(url, View::Inspect, Vec::new(), cancellation)
                .await?
            {
                Response::Page {
                    url,
                    html,
                    resources,
                    session,
                } => Ok(BrowserPage {
                    url,
                    html,
                    resources,
                    session,
                }),
                Response::Download { url, .. } => Ok(BrowserPage {
                    url,
                    html: String::new(),
                    resources: Vec::new(),
                    session: Session::default(),
                }),
                _ => Err(Failure::Unavailable.into()),
            }
        })
    }

    fn read_session<'a>(
        &'a self,
        url: &'a Url,
        cancellation: &'a CancellationToken,
    ) -> ResolveFuture<'a, Session> {
        Box::pin(self.session(url, View::Session, cancellation))
    }

    fn verify<'a>(
        &'a self,
        url: &'a Url,
        locale: Locale,
        cancellation: &'a CancellationToken,
    ) -> ResolveFuture<'a, Session> {
        Box::pin(self.session(url, View::Verify(locale), cancellation))
    }
}

fn step(step: &RecipeStep) -> Step {
    match step {
        RecipeStep::Get(path) => Step::Get { path: path.clone() },
        RecipeStep::SubmitForm {
            selector,
            button,
            fields,
        } => Step::Submit {
            selector: selector.clone(),
            button: button.clone(),
            fields: fields.clone().into_iter().collect(),
        },
        RecipeStep::Wait {
            seconds,
            from_page,
            default_seconds,
        } => Step::Wait {
            seconds: *seconds,
            selector: from_page.clone(),
            default_seconds: *default_seconds,
        },
        RecipeStep::Extract {
            css,
            regex,
            attribute,
        } => Step::Extract {
            selector: css.clone(),
            regex: regex.clone(),
            attribute: attribute.clone(),
        },
        RecipeStep::Follow(_) => Step::Follow,
        RecipeStep::ExpectDownload(_) => Step::ExpectDownload,
        RecipeStep::Click { selector } => Step::Click {
            selector: selector.clone(),
        },
        RecipeStep::WaitFor { selector } => Step::WaitFor {
            selector: selector.clone(),
        },
    }
}

fn session_headers(session: &Session) -> reqwest::header::HeaderMap {
    // These cookies already passed the native URL, path and Secure checks.
    let mut headers = reqwest::header::HeaderMap::new();
    if !session.user_agent.is_empty()
        && let Ok(value) = HeaderValue::from_str(&session.user_agent)
    {
        headers.insert(header::USER_AGENT, value);
    }
    let pairs = session
        .cookies
        .iter()
        .filter_map(|value| value.split(';').next())
        .collect::<Vec<_>>();
    if !pairs.is_empty()
        && let Ok(mut value) = HeaderValue::from_str(&pairs.join("; "))
    {
        value.set_sensitive(true);
        headers.insert(header::COOKIE, value);
    }
    headers
}

#[derive(Clone, Default)]
pub(crate) struct Sessions(Arc<RwLock<HashMap<String, SiteSession>>>);

struct SiteSession {
    user_agent: HeaderValue,
    cookies: Jar,
    version: u64,
    referer: Option<HeaderValue>,
}

impl fmt::Debug for Sessions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sessions").finish_non_exhaustive()
    }
}

impl Sessions {
    pub(crate) fn store(&self, url: &Url, session: Session, verified: bool) {
        self.store_with_referer(url, session, None, verified);
    }

    pub(crate) fn store_with_referer(
        &self,
        url: &Url,
        session: Session,
        referer: Option<&str>,
        verified: bool,
    ) {
        let Ok(user_agent) = HeaderValue::from_str(&session.user_agent) else {
            return;
        };
        if user_agent.is_empty() {
            return;
        }
        let parsed_referer = referer
            .map(str::trim)
            .filter(|raw| Url::parse(raw).is_ok())
            .and_then(|raw| HeaderValue::from_str(raw).ok())
            .map(|mut value| {
                value.set_sensitive(true);
                value
            });
        let Ok(mut sites) = self.0.write() else {
            return;
        };
        let site = sites
            .entry(url.origin().ascii_serialization())
            .or_insert_with(|| SiteSession {
                user_agent: user_agent.clone(),
                cookies: Jar::default(),
                version: 0,
                referer: None,
            });
        site.user_agent = user_agent;
        if parsed_referer.is_some() {
            site.referer = parsed_referer;
        }
        for cookie in session.cookies {
            site.cookies.add_cookie_str(&cookie, url);
        }
        site.version += u64::from(verified);
    }

    pub(crate) fn version(&self, url: &Url) -> u64 {
        self.0
            .read()
            .ok()
            .and_then(|sites| {
                sites
                    .get(&url.origin().ascii_serialization())
                    .map(|site| site.version)
            })
            .unwrap_or_default()
    }

    pub(crate) fn headers(&self, url: &Url) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(sites) = self.0.read()
            && let Some(site) = sites.get(&url.origin().ascii_serialization())
        {
            headers.insert(header::USER_AGENT, site.user_agent.clone());
            if let Some(referer) = &site.referer {
                headers.insert(header::REFERER, referer.clone());
            }
            if let Some(mut value) = site.cookies.cookies(url) {
                value.set_sensitive(true);
                headers.insert(header::COOKIE, value);
            }
        }
        headers
    }

    pub(crate) fn clear(&self) {
        if let Ok(mut sites) = self.0.write() {
            sites.clear();
        }
    }
}
