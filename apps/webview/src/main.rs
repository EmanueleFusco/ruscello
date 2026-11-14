//! Native event loop for the on-demand browser helper.

mod dom;
mod job;

use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, Read as _, Write as _},
    path::{Path, PathBuf},
    sync::mpsc::{self, RecvTimeoutError, Sender},
    thread,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use ruscello_browser::protocol::{Failure, MAX_FRAME, Request, Response, Url, web_url};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    platform::run_return::EventLoopExtRunReturn as _,
    window::{Window, WindowBuilder},
};
use wry::{NewWindowResponse, PageLoadEvent, WebContext, WebView, WebViewBuilder};

use job::Job;

const TICK: Duration = Duration::from_millis(150);
#[cfg(target_os = "macos")]
const BLANK_PAGE: &str =
    "<!doctype html><html><head><title>Ruscello</title></head><body></body></html>";

#[derive(Debug)]
enum Message {
    Input(Request),
    Closed,
    Tick,
    LoadStarted,
    LoadFinished,
    BlankReady,
    Evaluated(u64, String),
    Download(Url),
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => {
            // Native errors can contain signed links and profile paths.
            let _ = reply(&Response::Error {
                kind: Failure::Unavailable,
            });
            std::process::ExitCode::FAILURE
        }
    }
}

fn profile_path() -> anyhow::Result<PathBuf> {
    let mut args = std::env::args_os().skip(1);
    anyhow::ensure!(args.next().is_some_and(|arg| arg == "--profile"));
    let path = args.next().context("missing browser profile")?;
    anyhow::ensure!(args.next().is_none());
    Ok(std::path::absolute(path)?)
}

fn lock_profile(path: &Path) -> anyhow::Result<Option<File>> {
    let mut directories = fs::DirBuilder::new();
    directories.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        directories.mode(0o700);
    }
    directories.create(path)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.join(".profile.lock"))?;
    match lock.try_lock() {
        Ok(()) => Ok(Some(lock)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn run() -> anyhow::Result<()> {
    let profile = profile_path()?;
    let Some(_lock) = lock_profile(&profile)? else {
        reply(&Response::Error {
            kind: Failure::ProfileInUse,
        })?;
        return Ok(());
    };
    let mut events = EventLoopBuilder::<Message>::with_user_event().build();
    let proxy = events.create_proxy();
    let window = WindowBuilder::new()
        .with_visible(false)
        .with_inner_size(LogicalSize::new(1000, 720))
        .build(&events)?;
    let mut context = WebContext::new(Some(profile.clone()));
    let webview = webview(&window, &mut context, &profile, &proxy)?;
    thread::Builder::new().name("browser-stdio".into()).spawn({
        let proxy = proxy.clone();
        move || read_requests(proxy)
    })?;
    let timer = spawn_timer(proxy.clone())?;
    let mut job = None::<Job>;
    let mut generation = 0;
    let mut blank_ready = false;
    let mut scheduled = None;
    events.run_return(|event, _, control| {
        let result = match event {
            Event::UserEvent(Message::Input(Request::Shutdown) | Message::Closed) => {
                *control = ControlFlow::Exit;
                return;
            }
            Event::UserEvent(Message::Input(request)) if job.is_none() => {
                generation += 1;
                match Job::start(request, generation, &window, &webview) {
                    Ok(active) => {
                        job = Some(active);
                        None
                    }
                    Err(response) => Some(*response),
                }
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => job.as_ref().map(|_| Response::Error {
                kind: Failure::Cancelled,
            }),
            Event::UserEvent(Message::LoadStarted) => {
                if let Some(active) = &mut job {
                    active.load_started();
                }
                if let Ok(address) = webview.url()
                    && let Ok(url) = Url::parse(&address)
                    && let Some(host) = url.host_str()
                    && let Some(active) = &job
                {
                    active.update_title(&window, host);
                }
                None
            }
            Event::UserEvent(Message::LoadFinished) => {
                if let Some(active) = &mut job {
                    active.load_finished();
                }
                None
            }
            Event::UserEvent(Message::BlankReady) => {
                blank_ready = true;
                None
            }
            Event::UserEvent(Message::Evaluated(id, payload)) => job
                .as_mut()
                .filter(|active| active.id == id)
                .and_then(|active| active.evaluated(&webview, &payload)),
            Event::UserEvent(Message::Download(url)) => job
                .as_mut()
                .and_then(|active| active.download(&webview, url)),
            Event::NewEvents(tao::event::StartCause::ResumeTimeReached { .. })
            | Event::UserEvent(Message::Tick) => job
                .as_mut()
                .and_then(|active| active.tick(&webview, &proxy, blank_ready)),
            _ => None,
        };
        if let Some(response) = result {
            if job.take().is_some() {
                window.set_visible(false);
                blank_ready = false;
                #[cfg(target_os = "macos")]
                let _ = webview.load_html(BLANK_PAGE);
                #[cfg(not(target_os = "macos"))]
                let _ = webview.load_url("about:blank");
            }
            if reply(&response).is_err() {
                *control = ControlFlow::Exit;
                return;
            }
        }
        let wake = job.as_ref().map(Job::wake_at);
        if wake != scheduled {
            scheduled = wake;
            let _ = timer.send(wake);
        }
        *control = wake.map_or(ControlFlow::Wait, ControlFlow::WaitUntil);
    });
    Ok(())
}

// `tao`'s GTK backend ignores `ControlFlow::WaitUntil` deadlines when idle.
fn spawn_timer(proxy: EventLoopProxy<Message>) -> io::Result<Sender<Option<Instant>>> {
    let (tx, rx) = mpsc::channel::<Option<Instant>>();
    thread::Builder::new()
        .name("browser-timer".into())
        .spawn(move || {
            let mut next = None;
            loop {
                let Some(deadline) = next else {
                    match rx.recv() {
                        Ok(updated) => next = updated,
                        Err(_) => break,
                    }
                    continue;
                };
                let now = Instant::now();
                if deadline > now {
                    match rx.recv_timeout(deadline - now) {
                        Ok(updated) => next = updated,
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                    continue;
                }
                if proxy.send_event(Message::Tick).is_err() {
                    break;
                }
                next = Some(now + TICK);
            }
        })?;
    Ok(tx)
}

fn read_requests(proxy: EventLoopProxy<Message>) {
    let mut input = io::stdin().lock();
    let mut frame = Vec::new();
    loop {
        frame.clear();
        if (&mut input)
            .take(MAX_FRAME as u64)
            .read_until(b'\n', &mut frame)
            .is_err()
            || frame.last() != Some(&b'\n')
        {
            break;
        }
        let Ok(request) = serde_json::from_slice(&frame) else {
            break;
        };
        if proxy.send_event(Message::Input(request)).is_err() {
            return;
        }
    }
    let _ = proxy.send_event(Message::Closed);
}

fn reply(response: &Response) -> io::Result<()> {
    let mut frame = serde_json::to_vec(response)?;
    if frame.len() >= MAX_FRAME {
        frame = serde_json::to_vec(&Response::Error {
            kind: Failure::InvalidRequest,
        })?;
    }
    let mut output = io::stdout().lock();
    output.write_all(&frame)?;
    output.write_all(b"\n")?;
    output.flush()
}

fn webview(
    window: &Window,
    context: &mut WebContext,
    _profile: &Path,
    proxy: &EventLoopProxy<Message>,
) -> anyhow::Result<WebView> {
    let load_proxy = proxy.clone();
    let download_proxy = proxy.clone();
    let builder = WebViewBuilder::new_with_web_context(context)
        .with_devtools(false)
        .with_navigation_handler(|target| {
            target == "about:blank" || Url::parse(&target).is_ok_and(|url| web_url(&url))
        })
        .with_permission_handler(|_| wry::PermissionResponse::Deny)
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .with_download_started_handler(move |target, _| {
            if let Ok(url) = Url::parse(&target)
                && web_url(&url)
            {
                let _ = download_proxy.send_event(Message::Download(url));
            }
            false
        })
        .with_on_page_load_handler(move |event, url| {
            if url == "about:blank" || url.is_empty() || url.starts_with("data:text/html") {
                if matches!(event, PageLoadEvent::Finished) {
                    let _ = load_proxy.send_event(Message::BlankReady);
                }
            } else {
                let _ = load_proxy.send_event(match event {
                    PageLoadEvent::Started => Message::LoadStarted,
                    PageLoadEvent::Finished => Message::LoadFinished,
                });
            }
        })
        .with_initialization_script_for_main_only(dom::CAPTURE_RESOURCES, true);
    #[cfg(target_os = "macos")]
    let builder = {
        use wry::WebViewBuilderExtDarwin as _;
        builder
            .with_html(BLANK_PAGE)
            .with_data_store_identifier(profile_identifier(_profile)?)
    };
    #[cfg(not(target_os = "macos"))]
    let builder = builder.with_url("about:blank");
    #[cfg(target_os = "linux")]
    {
        use tao::platform::unix::WindowExtUnix as _;
        use wry::WebViewBuilderExtUnix as _;
        let container = window.default_vbox().context("missing GTK container")?;
        Ok(builder.build_gtk(container)?)
    }
    #[cfg(not(target_os = "linux"))]
    Ok(builder.build(window)?)
}

#[cfg(target_os = "macos")]
fn profile_identifier(profile: &Path) -> anyhow::Result<[u8; 16]> {
    let path = profile.join("store-id");
    match fs::read(&path) {
        Ok(id) => id
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid WebKit profile ID")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut id = [0; 16];
            getrandom::fill(&mut id)
                .map_err(|_| anyhow::anyhow!("could not create WebKit profile ID"))?;
            id[6] = (id[6] & 0x0f) | 0x40;
            id[8] = (id[8] & 0x3f) | 0x80;
            let temporary = profile.join("store-id.tmp");
            let mut file = File::create(&temporary)?;
            file.write_all(&id)?;
            file.sync_all()?;
            fs::rename(temporary, path)?;
            Ok(id)
        }
        Err(error) => Err(error.into()),
    }
}
