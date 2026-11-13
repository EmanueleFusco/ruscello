use iced::{Size, Theme, futures::StreamExt, widget::text_editor, window};
use iced_test::{
    Simulator,
    runtime::{Action, task},
};
use ruscello_core::{Download, DownloadId, Source, Status};
use ruscello_engine::{EngineHandle, EngineOptions};
use ruscello_testkit::{FileSpec, TestServer};

use crate::{
    actions::Action as RowAction,
    app::{App, DETAILS_COLLAPSED_HEIGHT, DetailTab, Message},
    desktop::{Receiver, Startup},
    i18n::Strings,
    preferences::{Locale, Preferences},
    settings::Page,
};

fn receiver<T>() -> Receiver<T> {
    std::sync::Arc::new(tokio::sync::Mutex::new(tokio::sync::mpsc::channel(1).1))
}

fn app(temp: &tempfile::TempDir) -> anyhow::Result<App> {
    let (app, _) = App::boot(
        Preferences {
            locale: Locale::English,
            ..Preferences::default()
        },
        temp.path().join("gui.json"),
        temp.path().join("queue.db"),
        temp.path().join("downloads"),
        false,
        Strings::new(Locale::English)?,
        Startup {
            tray: receiver(),
            ipc: receiver(),
            inputs: Vec::new(),
        },
    );
    Ok(app)
}

fn simulator(app: &App) -> Simulator<'_, Message, Theme, iced::Renderer> {
    simulator_at(app, Size::new(1120.0, 720.0))
}

fn simulator_at(app: &App, size: Size) -> Simulator<'_, Message, Theme, iced::Renderer> {
    Simulator::with_size(
        iced::Settings {
            default_font: iced::Font::with_name("Inter Variable"),
            default_text_size: iced::Pixels(14.0),
            fonts: vec![
                (&include_bytes!("../assets/Inter-Regular.ttf")[..]).into(),
                crate::icons::FONT.into(),
            ],
            ..iced::Settings::default()
        },
        size,
        app.view(window::Id::unique()),
    )
}

fn click(app: &App, label: &str) -> anyhow::Result<Message> {
    let mut ui = simulator(app);
    ui.click(label)?;
    ui.into_messages()
        .last()
        .ok_or_else(|| anyhow::anyhow!("No message from {label}"))
}

async fn task_messages(task: iced::Task<Message>) -> Vec<Message> {
    let Some(mut actions) = task::into_stream(task) else {
        return Vec::new();
    };
    let mut messages = Vec::new();
    while let Some(action) = actions.next().await {
        if let Action::Output(message) = action {
            messages.push(message);
        }
    }
    messages
}

async fn apply(app: &mut App, message: Message) {
    let mut pending = std::collections::VecDeque::from([message]);
    while let Some(message) = pending.pop_front() {
        pending.extend(task_messages(app.update(message)).await);
    }
}

#[tokio::test]
async fn adds_pauses_resumes_and_removes_a_loopback_download() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let mut app = app(&temp)?;
    app.preferences.check_updates = false;
    let server = TestServer::start().await?;
    let file = server.file(
        FileSpec::random(11, 2 * 1024 * 1024)
            .ranges(true)
            .throttle_per_conn(32 * 1024),
    );
    let engine = EngineHandle::start(EngineOptions::new(
        temp.path().join("queue.db"),
        temp.path().join("downloads"),
    ))
    .await?;
    apply(&mut app, Message::Started(Some(engine.clone()))).await;
    apply(&mut app, Message::Snapshot(Some(engine.snapshot().await?))).await;
    let message = click(&app, "Add")?;
    let _ = app.update(message);
    let edit = app.update(Message::EditAdd(text_editor::Action::Edit(
        text_editor::Edit::Paste(std::sync::Arc::new(file.url().to_string())),
    )));
    for message in task_messages(edit).await {
        apply(&mut app, message).await;
    }
    let _ = app.update(Message::StartImmediately(false));
    let _ = app.update(Message::AddConnections("2".into()));
    let message = click(&app, "Add")?;
    apply(&mut app, message).await;
    let snapshot = engine.snapshot().await?;
    let id = snapshot
        .downloads
        .first()
        .map(|download| download.id)
        .ok_or_else(|| anyhow::anyhow!("Download was not queued"))?;
    assert_eq!(snapshot.downloads[0].status(), Status::Paused);
    assert_eq!(
        snapshot.downloads[0]
            .max_connections
            .map(std::num::NonZeroUsize::get),
        Some(2)
    );
    apply(&mut app, Message::Snapshot(Some(snapshot))).await;
    let name = crate::queue::display_name(&engine.snapshot().await?.downloads[0]);
    assert!(simulator(&app).find(name.as_str()).is_ok());
    let _ = app.update(Message::Select(id));
    apply(&mut app, Message::RowAction(RowAction::TogglePause)).await;
    let snapshot = engine.snapshot().await?;
    assert_ne!(snapshot.downloads[0].status(), Status::Paused);
    apply(&mut app, Message::Snapshot(Some(snapshot))).await;
    apply(&mut app, Message::RowAction(RowAction::TogglePause)).await;
    let snapshot = engine.snapshot().await?;
    assert_eq!(snapshot.downloads[0].status(), Status::Paused);
    apply(&mut app, Message::Snapshot(Some(snapshot))).await;
    let _ = app.update(Message::RowAction(RowAction::Cancel));
    assert!(
        simulator(&app)
            .find("Also delete downloaded and partial files")
            .is_ok()
    );
    let message = click(&app, "Remove")?;
    apply(&mut app, message).await;
    assert!(engine.snapshot().await?.downloads.is_empty());
    engine.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn filters_and_switches_the_view_language_without_restarting() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let mut app = app(&temp)?;
    app.booting = false;
    let id = DownloadId(1);
    let mut download = Download::new(
        id,
        Source::Http {
            url: "https://example.test/fixture.zip".into(),
        },
        temp.path().to_owned(),
    );
    download.filename = Some("fixture.zip".into());
    app.queue.order.push(id);
    app.queue.downloads.insert(id, download);
    app.queue.rebuild();
    assert!(simulator(&app).find("fixture.zip").is_ok());
    let _ = app.update(Message::Status(crate::queue::StatusFilter::Completed));
    assert!(
        simulator(&app)
            .find("No downloads match the filters.")
            .is_ok()
    );
    let _ = app.update(Message::Status(crate::queue::StatusFilter::Queued));
    assert!(simulator(&app).find("fixture.zip").is_ok());
    let _ = app.update(Message::Settings(true));
    let message = click(&app, "Italiano")?;
    apply(&mut app, message).await;
    assert!(simulator(&app).find("Impostazioni").is_ok());
    assert!(simulator(&app).find("Language").is_err());
    Ok(())
}

#[test]
fn captures_distinct_download_and_settings_snapshots() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let mut app = app(&temp)?;
    app.booting = false;
    let theme = app.theme(window::Id::unique()).unwrap_or(Theme::Light);
    let path = temp.path().join("download-view.sha256");
    let snapshot = simulator(&app).snapshot(&theme)?;
    assert!(snapshot.matches_hash(&path)?);
    assert!(simulator(&app).snapshot(&theme)?.matches_hash(&path)?);
    let _ = app.update(Message::Settings(true));
    assert!(!simulator(&app).snapshot(&theme)?.matches_hash(&path)?);
    Ok(())
}

#[cfg(windows)]
#[test]
fn nvidia_settings_offer_only_the_action_for_the_current_profile() -> anyhow::Result<()> {
    use crate::nvidia::{Action, Outcome, State};

    let temp = tempfile::tempdir()?;
    let mut app = app(&temp)?;
    let _ = app.update(Message::Settings(true));
    let _ = app.update(Message::NvidiaProfileResult(Outcome {
        state: State::External,
        changed: false,
    }));
    let mut ui = simulator(&app);
    assert!(
        ui.find("Fixed refresh rate is already set in the NVIDIA profile.")
            .is_ok()
    );
    assert!(ui.find("Restore previous NVIDIA settings").is_err());
    drop(ui);

    let _ = app.update(Message::NvidiaProfileResult(Outcome {
        state: State::Available,
        changed: false,
    }));
    assert!(matches!(
        click(&app, "Use fixed refresh rate")?,
        Message::NvidiaProfile(Action::Enable)
    ));
    let _ = app.update(Message::NvidiaProfileResult(Outcome {
        state: State::Managed,
        changed: true,
    }));
    assert!(matches!(
        click(&app, "Restore previous NVIDIA settings")?,
        Message::NvidiaProfile(Action::Restore)
    ));
    assert!(
        simulator(&app)
            .find("Restart Ruscello to apply the change.")
            .is_ok()
    );
    Ok(())
}

#[test]
fn switching_tabs_and_settings_pages_keeps_the_layout_still() -> anyhow::Result<()> {
    use std::time::{Duration, Instant};

    let temp = tempfile::tempdir()?;
    let mut app = app(&temp)?;
    app.booting = false;
    let toolbar = simulator(&app).find("Add")?.bounds();
    let details = simulator(&app).find("Details")?.bounds();
    for (label, expected) in [
        ("Segments", DetailTab::Segments),
        ("Log", DetailTab::Log),
        ("General", DetailTab::General),
    ] {
        let message = click(&app, label)?;
        assert!(matches!(message, Message::DetailTab(tab) if tab == expected));
        let _ = app.update(message);
        let mut ui = simulator(&app);
        assert_eq!(ui.find("Add")?.bounds(), toolbar);
        assert_eq!(ui.find("Details")?.bounds(), details);
        assert!(app.selection_opacity() < 1.0);
    }

    let _ = app.update(Message::Settings(true));
    let heading = simulator(&app).find("Settings")?.bounds();
    let navigation = simulator(&app).find("Downloads")?.bounds();
    let message = click(&app, "Speed")?;
    assert!(matches!(message, Message::SettingsPage(Page::Speed)));
    let _ = app.update(message);
    assert_eq!(simulator(&app).find("Settings")?.bounds(), heading);
    assert_eq!(simulator(&app).find("Downloads")?.bounds(), navigation);
    let _ = app.update(Message::AnimationFrame(
        Instant::now() + Duration::from_millis(200),
    ));
    assert_eq!(app.selection_opacity(), 1.0);

    let _ = app.update(Message::Motion(false));
    let _ = app.update(Message::SettingsPage(Page::General));
    assert_eq!(app.selection_opacity(), 1.0);
    Ok(())
}

#[test]
fn details_header_collapses_and_reopens_even_with_an_empty_queue() -> anyhow::Result<()> {
    use std::time::{Duration, Instant};

    let temp = tempfile::tempdir()?;
    let mut app = app(&temp)?;
    app.booting = false;
    let expanded = app.details_height();
    let start = Instant::now() + Duration::from_secs(1);
    let _ = app.update(Message::AnimationFrame(start));
    let message = click(&app, "Details")?;
    assert!(matches!(message, Message::ToggleDetails));
    let _ = app.update(message);
    assert!(!app.preferences.details);
    assert_eq!(app.details_height(), expanded);
    assert_eq!(app.details_tabs_opacity(), 1.0);
    let _ = app.update(Message::AnimationFrame(start + Duration::from_millis(32)));
    let initial_opacity = app.details_tabs_opacity();
    assert!(initial_opacity > 0.0 && initial_opacity < 1.0);
    let mut ui = simulator(&app);
    ui.click("Segments")?;
    assert!(
        !ui.into_messages()
            .any(|message| matches!(message, Message::DetailTab(_)))
    );
    let _ = app.update(Message::AnimationFrame(start + Duration::from_millis(64)));
    assert!(app.details_tabs_opacity() > 0.0);
    assert!(app.details_tabs_opacity() < initial_opacity);
    let _ = app.update(Message::AnimationFrame(start + Duration::from_millis(112)));
    assert!(app.details_height() > DETAILS_COLLAPSED_HEIGHT);
    assert_eq!(app.details_tabs_opacity(), 0.0);
    assert!(simulator(&app).find("Segments").is_err());

    let reopened = start + Duration::from_millis(160);
    let _ = app.update(Message::AnimationFrame(reopened));
    assert_eq!(app.details_height(), DETAILS_COLLAPSED_HEIGHT);

    let message = click(&app, "Details")?;
    assert!(matches!(message, Message::ToggleDetails));
    let _ = app.update(message);
    assert!(app.preferences.details);
    assert_eq!(app.details_tabs_opacity(), 0.0);
    let _ = app.update(Message::AnimationFrame(
        reopened + Duration::from_millis(32),
    ));
    assert!(app.details_tabs_opacity() > 0.0 && app.details_tabs_opacity() < 1.0);
    assert!(simulator(&app).find("Segments").is_ok());
    let _ = app.update(Message::Motion(false));
    assert_eq!(app.details_height(), expanded);
    assert_eq!(app.details_tabs_opacity(), 1.0);
    let _ = app.update(click(&app, "Details")?);
    assert_eq!(app.details_height(), DETAILS_COLLAPSED_HEIGHT);
    assert!(simulator(&app).find("Segments").is_err());
    Ok(())
}

#[test]
fn aligns_queue_headers_and_keeps_filenames_visible_at_minimum_width() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let mut app = app(&temp)?;
    app.booting = false;
    let _ = app.update(Message::Locale(Locale::Italian));
    let id = DownloadId(1);
    let mut download = Download::new(
        id,
        Source::Http {
            url: "https://example.test/fixture.zip".into(),
        },
        temp.path().to_owned(),
    );
    download.filename = Some("fixture.zip".into());
    download.total_bytes = Some(2_048);
    app.queue.order.push(id);
    app.queue.downloads.insert(id, download);
    app.queue.rebuild();

    let mut ui = simulator_at(&app, Size::new(950.0, 490.0));
    let header = ui.find(app.strings.get("size"))?.bounds();
    let value = ui
        .find(app.strings.with("kib", [("value", "2.0".into())]))?
        .bounds();
    assert!((header.x - value.x).abs() <= 1.0);
    let name = ui.find("fixture.zip")?;
    assert!(
        name.visible_bounds()
            .is_some_and(|bounds| bounds.width >= 65.0)
    );
    Ok(())
}

#[test]
fn advanced_add_labels_stay_visible_while_editing_in_italian() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let mut app = app(&temp)?;
    let _ = app.update(Message::Locale(Locale::Italian));
    let _ = app.update(Message::OpenAdd);
    let _ = app.update(Message::AddAdvanced);
    let _ = app.update(Message::AddConnections("4".into()));
    let _ = app.update(Message::AddChecksum("sha256:1234".into()));
    let _ = app.update(Message::AddBandwidth("1024".into()));
    let mut ui = simulator(&app);
    for (label, value) in [
        ("add-connections", "4"),
        ("checksum", "sha256:1234"),
        ("download-bandwidth", "1024"),
    ] {
        let label = ui.find(app.strings.get(label))?;
        assert!(label.visible_bounds().is_some());
        assert!(label.bounds().y < ui.find(value)?.bounds().y);
    }
    Ok(())
}

#[test]
fn preserves_forwarded_links_while_submitting_the_previous_batch() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let mut app = app(&temp)?;
    let _ = app.update(Message::OpenAdd);
    let dialog = app
        .add
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("Add dialog did not open"))?;
    dialog.submitting = true;
    let url = "https://example.test/next.zip";
    let _ = app.update(Message::Incoming(vec![url.into()]));
    let _ = app.update(Message::Added(true));
    let dialog = app
        .add
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Forwarded link did not open"))?;
    assert_eq!(dialog.text, url);
    assert!(!dialog.submitting);
    Ok(())
}
