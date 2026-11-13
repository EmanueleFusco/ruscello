#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod actions;
mod add;
mod app;
mod i18n;
mod icons;
mod preferences;
mod queue;
mod settings;
mod settings_view;
mod theme;
mod view;

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
            if std::env::var("ICED_BACKEND").as_deref() != Ok(backend) {
                // Pass the selected backend to the desktop process before iced starts.
                let status = std::process::Command::new(std::env::current_exe()?)
                    .args(std::env::args_os().skip(1))
                    .env("ICED_BACKEND", backend)
                    .status()?;
                anyhow::ensure!(status.success(), "Desktop process failed");
                return Ok(());
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

fn run(
    paths: AppPaths,
    config: std::path::PathBuf,
    preferences: Preferences,
    restore_failed: bool,
) -> anyhow::Result<()> {
    let _log = logging::init("ruscello")?;
    let strings = Strings::new(preferences.locale)?;
    let default_destination = directories::UserDirs::new()
        .and_then(|dirs| dirs.download_dir().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| paths.data().join("Downloads"));
    let database = paths.data().join("ruscello.db");
    let boot = move || {
        App::boot(
            preferences.clone(),
            config.clone(),
            database.clone(),
            default_destination.clone(),
            restore_failed,
            strings.clone(),
        )
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
    Ok(())
}
