use std::time::{Duration, Instant};

use cookie::Cookie;
use ruscello_browser::{
    import,
    protocol::{
        Failure, Locale, MAX_COOKIE_BYTES, MAX_HTML, MAX_IMPORT_COOKIES, Request, Response,
        Session, Step, Url, View, challenge_url, web_url,
    },
};
use tao::{event_loop::EventLoopProxy, window::Window};
use wry::WebView;

use crate::{Message, TICK, dom};

const SETTLE: Duration = Duration::from_millis(350);
const PAGE_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) struct Job {
    pub id: u64,
    pub next_tick: Instant,
    source: Url,
    view: View,
    steps: std::vec::IntoIter<Step>,
    current: Option<Step>,
    page: Url,
    user_agent: String,
    loading: bool,
    navigating: bool,
    evaluating: bool,
    deadline: Instant,
    step_deadline: Instant,
    extracted: Option<Url>,
}

impl Job {
    pub(super) fn start(
        request: Request,
        id: u64,
        window: &Window,
        webview: &WebView,
    ) -> Result<Self, Box<Response>> {
        let (source, view, steps) = match request {
            Request::Open { url, view, steps } if web_url(&url) && steps.len() <= 32 => {
                (url, view, steps)
            }
            Request::Import { url, cookies }
                if web_url(&url) && cookies.len() <= MAX_IMPORT_COOKIES =>
            {
                let count = cookies
                    .iter()
                    .filter(|value| value.len() <= MAX_COOKIE_BYTES)
                    .filter_map(|value| import::for_site(value, &url).ok())
                    .filter(|cookie| webview.set_cookie(cookie).is_ok())
                    .count();
                return Err(Box::new(Response::Imported { count }));
            }
            _ => return Err(Box::new(error(Failure::InvalidRequest))),
        };
        let now = Instant::now();
        let timeout = match view {
            View::Verify(locale) => {
                set_title(window, locale, source.host_str().unwrap_or_default());
                window.set_visible(true);
                window.set_focus();
                Duration::from_secs(590)
            }
            View::Resolve => Duration::from_secs(3700),
            View::Session | View::Inspect => PAGE_TIMEOUT,
        };
        let mut steps = steps.into_iter();
        if matches!(steps.as_slice().first(), Some(Step::Get { path }) if path.is_empty()) {
            steps.next();
        }
        let current = steps.next();
        Ok(Self {
            id,
            next_tick: now + SETTLE,
            page: source.clone(),
            source,
            view,
            current,
            steps,
            user_agent: String::new(),
            loading: false,
            navigating: false,
            evaluating: false,
            deadline: now + timeout,
            step_deadline: now
                + if matches!(view, View::Verify(_)) {
                    timeout
                } else {
                    PAGE_TIMEOUT
                },
            extracted: None,
        })
    }

    pub(super) fn load_started(&mut self) {
        self.loading = true;
    }

    pub(super) fn load_finished(&mut self) {
        self.loading = false;
        self.next_tick = Instant::now() + SETTLE;
    }

    pub(super) fn update_title(&self, window: &Window, host: &str) {
        if let View::Verify(locale) = self.view {
            set_title(window, locale, host);
        }
    }

    pub(super) fn wake_at(&self) -> Instant {
        self.next_tick.min(self.deadline).min(self.step_deadline)
    }

    pub(super) fn tick(
        &mut self,
        webview: &WebView,
        proxy: &EventLoopProxy<Message>,
        blank_ready: bool,
    ) -> Option<Response> {
        let now = Instant::now();
        if now >= self.deadline || now >= self.step_deadline {
            return Some(error(Failure::Timeout));
        }
        if now < self.next_tick {
            return None;
        }
        self.next_tick = now + TICK;
        if self.loading
            || self.evaluating
            || cfg!(any(target_os = "macos", target_os = "linux"))
                && !self.navigating
                && !blank_ready
        {
            return None;
        }
        let html = match &self.current {
            None => matches!(self.view, View::Inspect | View::Resolve),
            Some(Step::Extract { regex: Some(_), .. }) => true,
            _ => false,
        };
        let script = dom::evaluate(
            self.current.as_ref(),
            html,
            matches!(self.view, View::Inspect),
            &self.source.origin().ascii_serialization(),
        );
        let proxy = proxy.clone();
        let id = self.id;
        if webview
            .evaluate_script_with_callback(&script, move |value| {
                let _ = proxy.send_event(Message::Evaluated(id, value));
            })
            .is_err()
        {
            return Some(error(Failure::Unavailable));
        }
        self.evaluating = true;
        None
    }

    pub(super) fn evaluated(&mut self, webview: &WebView, value: &str) -> Option<Response> {
        self.evaluating = false;
        let Ok(mut snapshot) = serde_json::from_str::<dom::Snapshot>(value) else {
            return None;
        };
        if !snapshot.user_agent.is_empty() && snapshot.user_agent.len() <= 1024 {
            self.user_agent = std::mem::take(&mut snapshot.user_agent);
        }
        if matches!(self.view, View::Session) {
            if self.user_agent.is_empty() {
                return None;
            }
            return Some(
                self.session(webview, &self.source)
                    .map_or_else(error, |session| Response::Session { session }),
            );
        }
        if !self.navigating {
            if self.user_agent.is_empty() {
                return None;
            }
            if webview.load_url(self.source.as_str()).is_err() {
                return Some(error(Failure::Unavailable));
            }
            self.navigating = true;
            self.loading = true;
            return None;
        }
        if !snapshot.ready || !web_url(&snapshot.url) {
            return None;
        }
        if snapshot.challenge || challenge_url(&snapshot.url) {
            return if matches!(self.view, View::Verify(_)) {
                None
            } else {
                Some(error(Failure::VerificationRequired))
            };
        }
        if matches!(self.view, View::Verify(_)) {
            // A login or challenge redirect can visit other sites before returning.
            if snapshot.url.origin() != self.source.origin() || self.loading {
                return None;
            }
            return Some(
                self.session(webview, &self.source)
                    .map_or_else(error, |session| Response::Session { session }),
            );
        }
        self.page = snapshot.url.clone();
        if self.loading && snapshot.value.is_none() {
            return None;
        }
        if self.current.is_none() {
            if let Some(url) = self.extracted.take() {
                return self.download(webview, url);
            }
            if snapshot.html.len() > MAX_HTML {
                return Some(error(Failure::InvalidRequest));
            }
            let resources = snapshot
                .resources
                .iter()
                .filter_map(|value| Url::parse(value).ok())
                .filter(web_url)
                .collect();
            let session = self.session(webview, &snapshot.url);
            return Some(session.map_or_else(error, |session| Response::Page {
                url: snapshot.url,
                html: snapshot.html,
                resources,
                session,
            }));
        }
        if snapshot.url.origin() != self.source.origin() {
            return Some(error(Failure::NoDownload));
        }
        self.advance(webview, snapshot)
    }

    fn advance(&mut self, webview: &WebView, snapshot: dom::Snapshot) -> Option<Response> {
        let step = self.current.as_ref()?;
        let now = Instant::now();
        let mut delay = SETTLE;
        let mut navigate = None;
        match step {
            Step::Get { path } => navigate = self.page.join(path.trim()).ok(),
            Step::Follow => navigate = self.extracted.take(),
            Step::ExpectDownload => return None,
            Step::Click { .. } | Step::Submit { .. } | Step::WaitFor { .. } => {
                snapshot.value?;
            }
            Step::Wait {
                seconds,
                selector,
                default_seconds,
            } => {
                let seconds = seconds
                    .or_else(|| {
                        let value = snapshot.value.as_deref()?;
                        value
                            .split(|c: char| !c.is_ascii_digit())
                            .find(|part| !part.is_empty())?
                            .parse()
                            .ok()
                    })
                    .or(*default_seconds);
                let Some(seconds) = seconds else {
                    return if selector.is_some() {
                        None
                    } else {
                        Some(error(Failure::InvalidRequest))
                    };
                };
                if seconds > 3600 {
                    return Some(error(Failure::InvalidRequest));
                }
                delay = delay.max(Duration::from_secs(seconds));
            }
            Step::Extract { regex, .. } => {
                let value = if let Some(expression) = regex {
                    let Ok(regex) = regex::Regex::new(expression) else {
                        return Some(error(Failure::InvalidRequest));
                    };
                    regex
                        .captures(&snapshot.html)
                        .and_then(|captured| captured.get(1).or_else(|| captured.get(0)))
                        .map(|captured| captured.as_str().to_owned())
                } else {
                    snapshot.value
                };
                let value = value?;
                self.extracted = self.page.join(value.trim()).ok().filter(web_url);
                if self.extracted.is_none() {
                    return Some(error(Failure::NoDownload));
                }
            }
        }
        if matches!(step, Step::Get { .. } | Step::Follow) {
            let Some(url) = navigate.filter(web_url) else {
                return Some(error(Failure::NoDownload));
            };
            if webview.load_url(url.as_str()).is_err() {
                return Some(error(Failure::Unavailable));
            }
            self.loading = true;
        }
        self.current = self.steps.next();
        self.next_tick = now + delay;
        self.step_deadline = self.next_tick + PAGE_TIMEOUT;
        None
    }

    pub(super) fn download(&self, webview: &WebView, url: Url) -> Option<Response> {
        if !self.navigating || matches!(self.view, View::Session) || !web_url(&url) {
            return None;
        }
        let scope = if matches!(self.view, View::Verify(_)) {
            &self.source
        } else {
            &url
        };
        if matches!(self.view, View::Verify(_)) && url.origin() != self.source.origin() {
            return None;
        }
        let response = self.session(webview, scope).map_or_else(error, |session| {
            if matches!(self.view, View::Verify(_)) {
                Response::Session { session }
            } else {
                Response::Download {
                    url,
                    referer: self.page.clone(),
                    session,
                }
            }
        });
        Some(response)
    }

    fn session(&self, webview: &WebView, url: &Url) -> Result<Session, Failure> {
        let cookies = cookies_for_url(webview, url)?;
        Ok(Session {
            user_agent: self.user_agent.clone(),
            cookies: cookies
                .into_iter()
                .filter_map(|cookie| scoped(cookie, url))
                .take(512)
                .collect(),
        })
    }
}

fn cookies_for_url(webview: &WebView, url: &Url) -> Result<Vec<Cookie<'static>>, Failure> {
    // WKWebView's URL helper compares domain strings only and skips IP-address hosts.
    // Filter the native cookie list for all platforms below, including cookie paths.
    #[cfg(target_os = "macos")]
    let result = webview.cookies();
    #[cfg(not(target_os = "macos"))]
    let result = webview.cookies_for_url(url.as_str());
    #[cfg(target_os = "macos")]
    let _ = url;
    result.map_err(|_| Failure::Unavailable)
}

fn scoped(mut cookie: Cookie<'static>, url: &Url) -> Option<String> {
    let host = url.host_str()?;
    let expires = cookie.expires_datetime();
    if cookie.partitioned() == Some(true)
        || cookie.secure() == Some(true) && url.scheme() != "https"
        || expires.is_some_and(|expiry| expiry <= cookie::time::OffsetDateTime::now_utc())
        || cookie.domain().is_some_and(|domain| {
            host != domain
                && !host
                    .strip_suffix(domain)
                    .is_some_and(|prefix| prefix.ends_with('.'))
        })
    {
        return None;
    }
    let path = cookie.path().unwrap_or("/");
    if url.path() != path
        && !url
            .path()
            .strip_prefix(path)
            .is_some_and(|suffix| path.ends_with('/') || suffix.starts_with('/'))
    {
        return None;
    }
    // The resolver keys exported credentials by exact origin. Keep them host-only in its jar.
    cookie.unset_domain();
    let value = cookie.to_string();
    (value.len() <= MAX_COOKIE_BYTES).then_some(value)
}

fn error(kind: Failure) -> Response {
    Response::Error { kind }
}

fn set_title(window: &Window, locale: Locale, host: &str) {
    let title = match locale {
        Locale::En => "Verification required",
        Locale::It => "Verifica richiesta",
    };
    window.set_title(&format!("{title} — {host} — Ruscello"));
}
