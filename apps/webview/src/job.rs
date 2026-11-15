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
const EVAL_TIMEOUT: Duration = Duration::from_secs(2);
const VERIFY_IDLE_TICK: Duration = Duration::from_millis(250);

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
    evaluating: Option<Instant>,
    eval_seq: u64,
    saw_challenge: bool,
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
                let _ = webview.focus();
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
        let next_tick = if matches!(view, View::Verify(_)) {
            now
        } else {
            now + SETTLE
        };
        Ok(Self {
            id,
            next_tick,
            page: source.clone(),
            source,
            view,
            current,
            steps,
            user_agent: String::new(),
            loading: false,
            navigating: false,
            evaluating: None,
            eval_seq: 0,
            saw_challenge: false,
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
        self.evaluating = None;
        self.eval_seq = self.eval_seq.wrapping_add(1);
    }

    pub(super) fn load_finished(&mut self, webview: &WebView) {
        self.loading = false;
        self.evaluating = None;
        self.eval_seq = self.eval_seq.wrapping_add(1);
        self.next_tick = Instant::now() + SETTLE;
        if matches!(self.view, View::Verify(_)) {
            let _ = webview.focus();
        }
    }

    pub(super) fn new_window(&mut self, webview: &WebView, target: Url) -> Option<Response> {
        if !matches!(self.view, View::Verify(_) | View::Resolve)
            || (!same_site(&self.page, &target) && !same_site(&self.source, &target))
        {
            return None;
        }
        let _ = webview.load_url(target.as_str());
        self.page = target;
        self.navigating = true;
        self.loading = true;
        self.evaluating = None;
        self.eval_seq = self.eval_seq.wrapping_add(1);
        if matches!(self.view, View::Verify(_)) {
            let _ = webview.focus();
        }
        None
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
        if self
            .evaluating
            .is_some_and(|started| now.saturating_duration_since(started) >= EVAL_TIMEOUT)
        {
            self.evaluating = None;
            self.eval_seq = self.eval_seq.wrapping_add(1);
        }
        if now < self.next_tick {
            return None;
        }
        self.next_tick = now
            + if matches!(self.view, View::Verify(_)) && self.navigating && !self.loading {
                VERIFY_IDLE_TICK
            } else {
                TICK
            };
        if self.loading
            || self.evaluating.is_some()
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
        self.eval_seq = self.eval_seq.wrapping_add(1);
        let seq = self.eval_seq;
        if webview
            .evaluate_script_with_callback(&script, move |value| {
                let _ = proxy.send_event(Message::Evaluated(id, seq, value));
            })
            .is_err()
        {
            return Some(error(Failure::Unavailable));
        }
        self.evaluating = Some(now);
        None
    }

    pub(super) fn evaluated(
        &mut self,
        webview: &WebView,
        seq: u64,
        value: &str,
    ) -> Option<Response> {
        if seq != self.eval_seq {
            return None;
        }
        self.evaluating = None;
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
        self.page = snapshot.url.clone();
        if snapshot.challenge || challenge_url(&snapshot.url) {
            self.saw_challenge = true;
            return if matches!(self.view, View::Verify(_)) {
                None
            } else {
                Some(error(Failure::VerificationRequired))
            };
        }
        if matches!(self.view, View::Verify(_)) {
            // A login or challenge redirect can visit other sites before returning.
            // If no challenge appeared on the landing page (e.g., step 2 Turnstile),
            // keep the window open until a challenge is solved or a download starts.
            if !self.saw_challenge || snapshot.url.origin() != self.source.origin() || self.loading
            {
                return None;
            }
            return Some(
                self.session(webview, &self.source)
                    .map_or_else(error, |session| Response::Session { session }),
            );
        }
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
        let response =
            self.session(webview, &url)
                .map_or_else(error, |session| Response::Download {
                    url,
                    referer: self.page.clone(),
                    session,
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

fn same_site(left: &Url, right: &Url) -> bool {
    let (Some(a), Some(b)) = (left.host_str(), right.host_str()) else {
        return false;
    };
    let a = a.trim_end_matches('.').to_ascii_lowercase();
    let b = b.trim_end_matches('.').to_ascii_lowercase();
    if a == b {
        return true;
    }
    match (site_root(&a), site_root(&b)) {
        (Some(root_a), Some(root_b)) => root_a == root_b,
        _ => false,
    }
}

fn site_root(host: &str) -> Option<&str> {
    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        return Some(host);
    }
    let labels: Vec<&str> = host.split('.').filter(|label| !label.is_empty()).collect();
    if labels.len() < 2 {
        return None;
    }
    let tld = labels[labels.len() - 1];
    let sld = labels[labels.len() - 2];
    let take = if labels.len() >= 3
        && tld.len() == 2
        && matches!(sld, "co" | "com" | "net" | "org" | "gov" | "edu")
    {
        3
    } else {
        2
    };
    let offset = host
        .match_indices('.')
        .nth_back(take - 1)
        .map_or(0, |(index, _)| index + 1);
    Some(&host[offset..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_same_site_and_rejects_third_party_popups() -> Result<(), Box<dyn std::error::Error>>
    {
        let page = Url::parse("https://datanodes.to/abc/file.rar")?;
        assert!(same_site(
            &page,
            &Url::parse("https://dl3.datanodes.to/d/123")?
        ));
        assert!(same_site(
            &page,
            &Url::parse("https://datanodes.to/download")?
        ));
        assert!(!same_site(&page, &Url::parse("https://other.to/download")?));
        assert!(!same_site(
            &page,
            &Url::parse("https://ads.example.net/pop")?
        ));
        let co_uk = Url::parse("https://dl.example.co.uk/file")?;
        assert!(same_site(&co_uk, &Url::parse("https://example.co.uk/dl")?));
        assert!(!same_site(&co_uk, &Url::parse("https://other.co.uk/ad")?));
        Ok(())
    }
}
