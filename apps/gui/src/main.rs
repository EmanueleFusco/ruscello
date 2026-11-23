#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod actions;
mod add;
mod app;
mod desktop;
mod i18n;
mod icons;
mod instance;
#[cfg(target_os = "macos")]
mod macos_open;
#[cfg(windows)]
mod nvidia;
mod preferences;
mod queue;
#[cfg(windows)]
mod renderer;
mod settings;
mod settings_view;
mod theme;
mod updater;
mod view;

#[cfg(test)]
#[path = "../tests/workflows.rs"]
mod workflows;

use std::process::ExitCode;

use ruscello_platform::{logging, paths::AppPaths};

use crate::{
    app::App,
    i18n::Strings,
    preferences::{Locale, Preferences},
};

fn main() -> ExitCode {
    // The renderer must be selected before iced and the logging worker start.
    let result = AppPaths::discover()
        .map_err(anyhow::Error::from)
        .and_then(|paths| {
            let config = paths.config().join("gui.json");
            let loaded = Preferences::load(&config);
            let restore_failed = loaded.is_err();
            let preferences = loaded.unwrap_or_default();
            let backend = preferences.renderer.backend();
            let gpu = preferences.renderer == preferences::Renderer::Hardware;
            let backend_matches = std::env::var("ICED_BACKEND").as_deref() == Ok(backend);
            let present_matches =
                !gpu || std::env::var("ICED_PRESENT_MODE").as_deref() == Ok("fifo");
            if !backend_matches || !present_matches {
                #[cfg(windows)]
                renderer::configure(preferences.renderer)?;
                #[cfg(unix)]
                return exec_with_renderer(backend, gpu);
            }
            run(paths, config, preferences, restore_failed)
        });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::debug!(%error, "Desktop startup failed");
            if let Ok(strings) = Strings::new(Locale::System) {
                eprintln!("{}", strings.get("start-error"));
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(unix)]
fn exec_with_renderer(backend: &str, gpu: bool) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .args(std::env::args_os().skip(1))
        .env("ICED_BACKEND", backend);
    if gpu {
        command.env("ICED_PRESENT_MODE", "fifo");
    }
    Err(command.exec().into())
}

fn run(
    paths: AppPaths,
    config: std::path::PathBuf,
    preferences: Preferences,
    restore_failed: bool,
) -> anyhow::Result<()> {
    let _log = logging::init("ruscello")?;
    let inputs = std::env::args_os()
        .skip(1)
        .filter(|arg| arg != "--startup" && arg != "--update-failed")
        .map(|arg| instance::normalize(&arg))
        .collect::<Vec<_>>();
    let Some(primary) = instance::start(paths.data(), inputs.clone())? else {
        return Ok(());
    };
    #[cfg(target_os = "macos")]
    let native_open = macos_open::OpenEvents::install(primary.native_sender())?;
    let events = desktop::event_bridge();
    let strings = Strings::new(preferences.locale)?;
    let default_destination = directories::UserDirs::new()
        .and_then(|dirs| dirs.download_dir().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| paths.data().join("Downloads"));
    let database = paths.data().join("ruscello.db");
    let restart = updater::Restart::default();
    let app_restart = restart.clone();
    let receiver = primary.receiver();
    let boot = move || {
        let (mut app, task) = App::boot(
            preferences.clone(),
            config.clone(),
            database.clone(),
            default_destination.clone(),
            restore_failed,
            strings.clone(),
            desktop::Startup {
                tray: events.clone(),
                ipc: receiver.clone(),
                inputs: inputs.clone(),
            },
        );
        app.restart = app_restart.clone();
        if std::env::var_os("RUSCELLO_UPDATE_FAILED").is_some()
            || std::env::args_os().any(|arg| arg == "--update-failed")
        {
            app.banner = Some("update-install-error");
        }
        (app, task)
    };
    iced::daemon(boot, App::update, App::view)
        .title(App::title)
        .subscription(App::subscription)
        .theme(App::theme)
        .settings(iced::Settings {
            default_text_size: iced::Pixels(14.0),
            ..iced::Settings::default()
        })
        .default_font(iced::Font::with_name("Inter Variable"))
        .font(&include_bytes!("../assets/Inter-Regular.ttf")[..])
        .font(icons::FONT)
        .run()?;
    #[cfg(target_os = "macos")]
    drop(native_open);
    let prepared = restart
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(update) = prepared {
        let (next, failed) = match update.apply() {
            Ok(Some(relaunch)) => (relaunch, false),
            Ok(None) => return Ok(()), // The Windows installer relaunches after replacement.
            Err(error) => {
                tracing::debug!(%error, "Could not apply the verified update");
                (updater::Relaunch::new(updater::relaunch_path()?), true)
            }
        };
        // Release the IPC lock before the new instance tries to start.
        drop(primary);
        drop(update);
        let command = relaunch_command(&next.executable, failed);
        if let Some(restored) = next.launch(command)? {
            relaunch_command(&restored, true).spawn()?;
        }
    }
    Ok(())
}

fn relaunch_command(path: &std::path::Path, failed: bool) -> std::process::Command {
    let mut command = std::process::Command::new(path);
    command.env_remove("RUSCELLO_UPDATE_FAILED");
    #[cfg(target_os = "macos")]
    command.env_remove(updater::STARTUP_MARKER);
    if failed {
        command.env("RUSCELLO_UPDATE_FAILED", "1");
    }
    #[cfg(target_os = "linux")]
    {
        // The new AppImage runtime supplies its own mount and executable paths.
        command.env_remove("APPDIR").env_remove("APPIMAGE");
        if let Some(directory) = std::env::var_os("OWD") {
            command.current_dir(directory);
        }
    }
    command
}
