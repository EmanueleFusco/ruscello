use std::sync::Arc;

#[cfg(any(windows, target_os = "linux"))]
use std::path::PathBuf;

use iced::futures::{Stream, stream};
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem},
};

use crate::{app::Message, i18n::Strings};

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
    let mut pixels = Vec::with_capacity(24 * 24 * 4);
    for y in 0..24_i32 {
        for x in 0..24_i32 {
            let arrow = ((10..=13).contains(&x) && (5..=13).contains(&y))
                || ((10..=15).contains(&y) && (x - 11).abs() + y - 10 <= 5)
                || ((5..=18).contains(&x) && (17..=19).contains(&y));
            pixels.extend(if arrow {
                [255, 255, 255, 255]
            } else {
                [0, 105, 100, 255]
            });
        }
    }
    let menu = Menu::new();
    for (id, label) in [
        ("ruscello-show", "tray-show"),
        ("ruscello-pause", "pause-all"),
        ("ruscello-quit", "tray-quit"),
    ] {
        menu.append(&MenuItem::with_id(id, strings.get(label), true, None))?;
    }
    Ok(TrayIconBuilder::new()
        .with_icon(Icon::from_rgba(pixels, 24, 24)?)
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
        "[Desktop Entry]\nName=Ruscello\nType=Application\nExec=\"{exe}\" %U\nTerminal=false\nMimeType={};\n",
        types.join(";")
    );
    fs::write(applications.join("org.ruscello.Ruscello.desktop"), entry)?;
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
            .args(["default", "org.ruscello.Ruscello.desktop", mime])
            .status()?;
        anyhow::ensure!(status.success(), "xdg-mime could not set the association");
    }
    Ok(())
}
