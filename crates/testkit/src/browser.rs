use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use tokio::sync::Notify;

use crate::{
    FileSpec, HeaderMap, HeaderValue, Method, Reply, Request, StatusCode, TestFile, TestServer,
    header,
};

/// Loopback verification and download pages shared by native WebView tests.
#[derive(Debug)]
pub struct BrowserFixture {
    pub protected: Vec<String>,
    pub recipe: String,
    pub resources: String,
    pub payload: TestFile,
    gate: Arc<Gate>,
    popups: Arc<AtomicUsize>,
}

#[derive(Debug, Default)]
struct Gate {
    visible: Notify,
    allow_click: AtomicBool,
    clicks: AtomicUsize,
    user_agent: Mutex<Option<HeaderValue>>,
    authorized_requests: AtomicUsize,
}

impl BrowserFixture {
    pub fn register(server: &TestServer) -> Self {
        let gate = Arc::new(Gate::default());
        let mut protected = Vec::new();
        let payload = server.file(
            FileSpec::random(59, 256 * 1024)
                .filename("browser.bin")
                .content_type("application/octet-stream")
                .content_disposition("attachment; filename=browser.bin"),
        );
        for index in 0..5 {
            let file = server.file(
                FileSpec::random(59, 256 * 1024)
                    .filename(format!("part-{index}.bin"))
                    .content_type("application/octet-stream")
                    .content_disposition(format!("attachment; filename=part-{index}.bin")),
            );
            let gate = gate.clone();
            protected.push(server.route(&format!("/protected/{index}"), move |request| {
                if verified(&gate, &request) {
                    gate.authorized_requests.fetch_add(1, Ordering::Relaxed);
                    return Reply::File(file.clone());
                }
                let Reply::Response(_, mut headers, page) = Reply::html(format!(r#"
                    <title>Verification fixture</title>
                    <form id="challenge-form" action="/verify" method="post">
                        <button id="verify" type="submit">Verify</button>
                        <input name="part" value="{index}" type="hidden">
                    </form>
                    <script>
                    const timer = setInterval(async () => {{
                        const response = await fetch('/verification-test-gate', {{cache: 'no-store'}});
                        if (response.ok) {{ clearInterval(timer); document.getElementById('verify').click(); }}
                    }}, 50);
                    </script>
                "#)) else { return Reply::status(StatusCode::INTERNAL_SERVER_ERROR); };
                headers.insert("cf-mitigated", HeaderValue::from_static("challenge"));
                Reply::Response(StatusCode::FORBIDDEN, headers, page)
            }));
        }
        server.route("/verification-test-gate", {
            let gate = gate.clone();
            move |_| {
                gate.visible.notify_one();
                Reply::status(if gate.allow_click.swap(false, Ordering::AcqRel) {
                    StatusCode::OK
                } else {
                    StatusCode::CONFLICT
                })
            }
        });
        server.route("/verify", {
            let gate = gate.clone();
            move |request| {
                let Some(part) = request
                    .body
                    .strip_prefix(b"part=")
                    .and_then(|value| std::str::from_utf8(value).ok())
                    .filter(|part| matches!(*part, "0" | "1" | "2" | "3" | "4"))
                else {
                    return Reply::status(StatusCode::BAD_REQUEST);
                };
                gate.clicks.fetch_add(1, Ordering::Relaxed);
                if let Ok(mut user_agent) = gate.user_agent.lock() {
                    *user_agent = request.headers.get(header::USER_AGENT).cloned();
                }
                let mut headers = crate::HeaderMap::new();
                headers.insert(
                    header::SET_COOKIE,
                    HeaderValue::from_static(
                        "fixture_verified=1; Path=/; HttpOnly; Max-Age=86400; SameSite=Lax",
                    ),
                );
                if let Ok(location) = HeaderValue::from_str(&format!("/protected/{part}")) {
                    headers.insert(header::LOCATION, location);
                }
                Reply::Response(StatusCode::SEE_OTHER, headers, String::new())
            }
        });
        let popups = Arc::new(AtomicUsize::new(0));
        for path in ["/popup-one", "/popup-two", "/popup-three"] {
            let popups = popups.clone();
            server.route(path, move |_| {
                popups.fetch_add(1, Ordering::Relaxed);
                Reply::html("Unexpected popup")
            });
        }
        server.route("/intercept", {
            let payload = payload.clone();
            move |request| {
                if request
                    .headers
                    .get(header::COOKIE)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.contains("fixture_download=1"))
                    && request
                        .headers
                        .get(header::REFERER)
                        .and_then(|value| value.to_str().ok())
                        .is_some_and(|value| value.ends_with("/browser-recipe"))
                {
                    Reply::File(payload.clone())
                } else {
                    Reply::status(StatusCode::FORBIDDEN)
                }
            }
        });
        let recipe = server.route("/browser-recipe", |_| {
            let Reply::Response(status, mut headers, body) = Reply::html(r#"
                <form action="/browser-recipe" onsubmit="return false">
                    <button type="button" id="download">Download</button>
                </form>
                <script>
                document.getElementById('download').onclick = () => {
                    window.open('/popup-one'); window.open('/popup-two'); window.open('/popup-three');
                    location.href = '/intercept';
                };
                </script>
            "#) else { return Reply::status(StatusCode::INTERNAL_SERVER_ERROR); };
            headers.insert(header::SET_COOKIE, HeaderValue::from_static("fixture_download=1; Path=/intercept; HttpOnly"));
            Reply::Response(status, headers, body)
        });
        server.route("/stream.m3u8", |_| Reply::status(StatusCode::OK));
        server.route("/xhr.mpd", |_| Reply::status(StatusCode::OK));
        let resources = server.route("/resources", |_| Reply::html(r#"
            <video><source src="/video.mp4"></video>
            <script>fetch('/stream.m3u8'); const xhr = new XMLHttpRequest(); xhr.open('GET', '/xhr.mpd'); xhr.send();</script>
        "#));
        Self {
            protected,
            recipe,
            resources,
            payload,
            gate,
            popups,
        }
    }

    pub async fn verification_visible(&self) {
        self.gate.visible.notified().await;
    }

    /// Simulates the user's click using the fixture document's own button and form action.
    pub fn click_verification(&self) {
        self.gate.allow_click.store(true, Ordering::Release);
    }

    pub fn verifications(&self) -> usize {
        self.gate.clicks.load(Ordering::Relaxed)
    }

    pub fn authorized_requests(&self) -> usize {
        self.gate.authorized_requests.load(Ordering::Relaxed)
    }

    pub fn popup_requests(&self) -> usize {
        self.popups.load(Ordering::Relaxed)
    }
}

fn verified(gate: &Gate, request: &Request) -> bool {
    request
        .headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("fixture_verified=1"))
        && gate.user_agent.lock().is_ok_and(|expected| {
            expected.is_some() && expected.as_ref() == request.headers.get(header::USER_AGENT)
        })
}

/// A batch of XFileSharing-like pages that reject early downloads and count popup requests.
#[derive(Debug)]
pub struct XfsBrowserFixture {
    pub shares: Vec<String>,
    pub files: Vec<TestFile>,
    popups: Arc<AtomicUsize>,
    early: Arc<AtomicUsize>,
}

impl XfsBrowserFixture {
    pub fn register(server: &TestServer) -> Self {
        use std::time::{Duration, Instant};

        const COUNTDOWN_SECONDS: u64 = 1;
        let popups = Arc::new(AtomicUsize::new(0));
        for name in ["one", "two", "three"] {
            let popups = popups.clone();
            server.route(&format!("/xfs-popup/{name}"), move |_| {
                popups.fetch_add(1, Ordering::Relaxed);
                Reply::html("Unexpected popup")
            });
        }
        let early = Arc::new(AtomicUsize::new(0));
        let mut shares = Vec::new();
        let mut files = Vec::new();
        for index in 0..10 {
            let stage = format!("/xfs/{index}/countdown");
            let transfer = format!("/xfs/{index}/attachment");
            let filename = format!("xfs-{index}.bin");
            let file = server.file(
                FileSpec::random(index, 4096)
                    .filename(&filename)
                    .content_disposition(format!("attachment; filename={filename}"))
                    .content_type("application/octet-stream"),
            );
            let began = Arc::new(Mutex::new(None::<Instant>));
            shares.push(server.route(&format!("/xfs/{index}"), {
                let stage = stage.clone();
                move |request| {
                    // Force the resolver down the real built-in XFS browser fallback.
                    if request.headers.get(header::USER_AGENT)
                        .and_then(|ua| ua.to_str().ok())
                        .is_some_and(|ua| ua.starts_with("Ruscello/"))
                    {
                        let mut headers = HeaderMap::new();
                        headers.insert("cf-mitigated", HeaderValue::from_static("challenge"));
                        return Reply::Response(StatusCode::FORBIDDEN, headers, String::new());
                    }
                    Reply::html(format!(r#"<form action="{stage}" method="post">
                        <input type="hidden" name="op" value="download1">
                        <button id="method_free" name="method_free" value="Free Download">Free Download</button>
                    </form>"#))
                }
            }));
            server.route(&stage, {
                let began = began.clone();
                let transfer = transfer.clone();
                move |request| {
                    if request.method != Method::POST || !request.body.split(|&byte| byte == b'&').any(|pair| pair == b"op=download1") {
                        return Reply::status(StatusCode::BAD_REQUEST);
                    }
                    let Ok(mut started) = began.lock() else { return Reply::status(StatusCode::INTERNAL_SERVER_ERROR); };
                    *started = Some(Instant::now());
                    Reply::html(format!(r#"
                        <div id="countdown" data-seconds="{COUNTDOWN_SECONDS}">{COUNTDOWN_SECONDS}</div>
                        <a id="downloadbtn" href="{transfer}">Download</a>
                        <script>
                        document.getElementById('downloadbtn').onclick = () => {{
                            window.open('/xfs-popup/one'); window.open('/xfs-popup/two'); window.open('/xfs-popup/three');
                        }};
                        </script>
                    "#))
                }
            });
            server.route(&transfer, {
                let file = file.clone();
                let early = early.clone();
                move |_| {
                    if began.lock().is_ok_and(|started| {
                        started.is_some_and(|time| {
                            time.elapsed() >= Duration::from_secs(COUNTDOWN_SECONDS)
                        })
                    }) {
                        Reply::File(file.clone())
                    } else {
                        early.fetch_add(1, Ordering::Relaxed);
                        Reply::status(StatusCode::CONFLICT)
                    }
                }
            });
            files.push(file);
        }
        Self {
            shares,
            files,
            popups,
            early,
        }
    }

    pub fn popup_requests(&self) -> usize {
        self.popups.load(Ordering::Relaxed)
    }

    pub fn early_requests(&self) -> usize {
        self.early.load(Ordering::Relaxed)
    }
}
