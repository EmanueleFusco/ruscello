#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod i18n;
mod icons;
mod preferences;
mod queue;
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
    match run() {
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

fn run() -> anyhow::Result<()> {
    let paths = AppPaths::discover()?;
    let config = paths.config().join("gui.json");
    let loaded = Preferences::load(&config);
    let restore_failed = loaded.is_err();
    let preferences = loaded.unwrap_or_default();
    let _log = logging::init("ruscello")?;
    let strings = Strings::new(preferences.locale)?;
    let destination = directories::UserDirs::new()
        .and_then(|dirs| dirs.download_dir().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| paths.data().join("Downloads"));
    let database = paths.data().join("ruscello.db");
    let boot = move || {
        App::boot(
            preferences,
            config.clone(),
            database.clone(),
            destination.clone(),
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
