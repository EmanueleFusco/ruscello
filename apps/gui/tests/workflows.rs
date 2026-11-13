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
    app::{App, Message},
    desktop::{Receiver, Startup},
    i18n::Strings,
    preferences::{Locale, Preferences},
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
        Size::new(1120.0, 720.0),
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
