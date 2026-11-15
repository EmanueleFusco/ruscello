use std::sync::Arc;

#[cfg(any(windows, target_os = "linux"))]
use std::path::PathBuf;

use iced::futures::{Stream, stream};
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem},
};

use crate::{app::Message, i18n::Strings, icons};

#[cfg(target_os = "linux")]
pub(crate) const APP_ID: &str = "org.ruscello.Ruscello";

#[derive(Debug, Clone, Copy)]
pub(crate) enum Event {
    Show,
    PauseAll,
    Quit,
}

pub(crate) type Receiver<T> = Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<T>>>;

pub(crate) struct Startup {
    pub(crate) tray: Receiver<Event>,
    pub(crate) ipc: Receiver<Vec<String>>,
    pub(crate) inputs: Vec<String>,
}

pub(crate) fn events<T: Send + 'static>(receiver: &Receiver<T>) -> impl Stream<Item = T> + use<T> {
    stream::unfold(receiver.clone(), |receiver| async move {
        let value = receiver.lock().await.recv().await?;
        Some((value, receiver))
    })
}

pub(crate) fn event_bridge() -> Receiver<Event> {
    let (sender, receiver) = tokio::sync::mpsc::channel(32);
    let menu_sender = sender.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let event = match event.id.0.as_str() {
            "ruscello-show" => Event::Show,
            "ruscello-pause" => Event::PauseAll,
            "ruscello-quit" => Event::Quit,
            _ => return,
        };
        let _ = menu_sender.try_send(event);
    }));
    TrayIconEvent::set_event_handler(Some(move |event| {
        if matches!(
            event,
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
        ) {
            let _ = sender.try_send(Event::Show);
        }
    }));
    Arc::new(tokio::sync::Mutex::new(receiver))
}

pub(crate) fn tray(strings: &Strings) -> anyhow::Result<TrayIcon> {
    let menu = Menu::new();
    for (id, label) in [
        ("ruscello-show", "tray-show"),
        ("ruscello-pause", "pause-all"),
        ("ruscello-quit", "tray-quit"),
    ] {
        menu.append(&MenuItem::with_id(id, strings.get(label), true, None))?;
    }
    Ok(TrayIconBuilder::new()
        .with_icon(Icon::from_rgba(
            icons::APP_ICON_RGBA.to_vec(),
            icons::APP_ICON_SIZE,
            icons::APP_ICON_SIZE,
        )?)
        .with_tooltip(strings.get("app-name"))
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()?)
}

pub(crate) async fn notify(title: String, body: String) -> Message {
    let _ = tokio::task::spawn_blocking(move || {
        if notify_rust::Notification::new()
            .summary(&title)
            .body(&body)
            .show()
            .is_err()
        {
            tracing::debug!("Desktop notification unavailable");
        }
    })
    .await;
    Message::Noop
}

pub(crate) async fn autostart(enabled: bool) -> bool {
    tokio::task::spawn_blocking(move || {
        let result = (|| -> anyhow::Result<()> {
            let executable = std::env::current_exe()?;
            let app = auto_launch::AutoLaunchBuilder::new()
                .set_app_name("Ruscello")
                .set_app_path(
                    executable
                        .to_str()
                        .ok_or_else(|| anyhow::anyhow!("Invalid executable path"))?,
                )
                .set_args(&["--startup"])
                .build()?;
            if enabled {
                app.enable()?;
            } else if app.is_enabled()? {
                app.disable()?;
            }
            Ok(())
        })();
        result.is_ok()
    })
    .await
    .unwrap_or(false)
}

#[cfg(any(windows, target_os = "linux"))]
pub(crate) async fn associations() -> bool {
    tokio::task::spawn_blocking(|| {
        let result = std::env::current_exe()
            .map_err(anyhow::Error::from)
            .and_then(register_associations);
        if result.is_err() {
            tracing::debug!("Could not register desktop associations");
        }
        result.is_ok()
    })
    .await
    .unwrap_or(false)
}

#[cfg(windows)]
fn register_associations(exe: PathBuf) -> anyhow::Result<()> {
    use winreg::{RegKey, enums::HKEY_CURRENT_USER};
    let user = RegKey::predef(HKEY_CURRENT_USER);
    let (classes, _) = user.create_subkey("Software\\Classes")?;
    let command = format!("\"{}\" \"%1\"", exe.display());
    for (name, class) in [(".torrent", "Ruscello.Torrent"), (".dlc", "Ruscello.DLC")] {
        let (extension, _) = classes.create_subkey(name)?;
        let (programs, _) = extension.create_subkey("OpenWithProgids")?;
        programs.set_value(class, &"")?;
        if extension.get_value::<String, _>("").is_err() {
            extension.set_value("", &class)?;
        }
        let (app, _) = classes.create_subkey(format!("{class}\\shell\\open\\command"))?;
        app.set_value("", &command)?;
    }
    let (magnet, _) = classes.create_subkey("magnet")?;
    magnet.set_value("", &"URL:Magnet")?;
    magnet.set_value("URL Protocol", &"")?;
    let (handler, _) = magnet.create_subkey("shell\\open\\command")?;
    handler.set_value("", &command)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn register_associations(exe: PathBuf) -> anyhow::Result<()> {
    use std::{fs, process::Command};
    let dirs = directories::BaseDirs::new()
        .ok_or_else(|| anyhow::anyhow!("User data directory unavailable"))?;
    let applications = dirs.data_dir().join("applications");
    fs::create_dir_all(&applications)?;
    let exe = exe
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid executable path"))?;
    anyhow::ensure!(!exe.contains(['\n', '\r']), "Invalid executable path");
    let exe = exe
        .replace('\\', "\\\\\\\\")
        .replace('"', "\\\\\"")
        .replace('%', "%%")
        .replace('$', "\\\\$")
        .replace('`', "\\\\`");
    let types = [
        "x-scheme-handler/magnet",
        "application/x-bittorrent",
        "application/x-dlc",
    ];
    let entry = format!(
        "[Desktop Entry]\nName=Ruscello Download Manager\nType=Application\nExec=\"{exe}\" %U\nIcon=ruscello\nStartupWMClass={APP_ID}\nTerminal=false\nMimeType={};\n",
        types.join(";")
    );
    let icon_dir = dirs.data_dir().join("icons/hicolor/512x512/apps");
    fs::create_dir_all(&icon_dir)?;
    fs::write(
        icon_dir.join("ruscello.png"),
        include_bytes!("../assets/ruscello.png"),
    )?;
    let desktop_file = format!("{APP_ID}.desktop");
    fs::write(applications.join(&desktop_file), entry)?;
    let mime = dirs.data_dir().join("mime");
    let packages = mime.join("packages");
    fs::create_dir_all(&packages)?;
    fs::write(
        packages.join("org.ruscello.Ruscello.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<mime-info xmlns=\"http://www.freedesktop.org/standards/shared-mime-info\">\n  <mime-type type=\"application/x-dlc\">\n    <comment>Download Link Container</comment>\n    <glob pattern=\"*.dlc\"/>\n  </mime-type>\n</mime-info>\n",
    )?;
    let status = Command::new("update-mime-database").arg(mime).status()?;
    anyhow::ensure!(status.success(), "Could not update the MIME database");
    for mime in types {
        let status = Command::new("xdg-mime")
            .args(["default", &desktop_file, mime])
            .status()?;
        anyhow::ensure!(status.success(), "xdg-mime could not set the association");
    }
    Ok(())
}

pub(crate) fn apply_window_theme(raw_window_id: u64, dark: bool) {
    #[cfg(windows)]
    titlebar::apply(raw_window_id, dark);
    #[cfg(not(windows))]
    let _ = (raw_window_id, dark);
}

#[cfg(windows)]
mod titlebar {
    #![allow(unsafe_code)]

    use std::ffi::c_void;

    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_BORDER_COLOR: u32 = 34;
    const DWMWA_CAPTION_COLOR: u32 = 35;
    const DWMWA_TEXT_COLOR: u32 = 36;

    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: *mut c_void,
            attribute: u32,
            value: *const c_void,
            size: u32,
        ) -> i32;
    }

    pub(super) fn apply(raw_window_id: u64, dark: bool) {
        if raw_window_id == 0 {
            return;
        }
        let hwnd = raw_window_id as usize as *mut c_void;
        let dark_mode: i32 = i32::from(dark);
        // COLORREF uses 0x00BBGGRR matching `theme.rs` palette background and text colors.
        let caption: u32 = if dark { 0x0021_1610 } else { 0x00f9_f4f1 };
        let text: u32 = if dark { 0x00f8_eee9 } else { 0x003a_2417 };
        let border: u32 = if dark { 0x003c_2b20 } else { 0x00e0_d6cd };
        // SAFETY: `raw_window_id` comes from `iced::window::raw_id` for this process's live window
        // HWND, and each attribute pointer references an aligned 4-byte scalar with exact byte size.
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                (&raw const dark_mode).cast(),
                size_of::<i32>() as u32,
            );
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_CAPTION_COLOR,
                (&raw const caption).cast(),
                size_of::<u32>() as u32,
            );
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_TEXT_COLOR,
                (&raw const text).cast(),
                size_of::<u32>() as u32,
            );
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR,
                (&raw const border).cast(),
                size_of::<u32>() as u32,
            );
        }
    }
}
