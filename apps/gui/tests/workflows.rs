use iced::{Size, Theme, futures::StreamExt, widget::text_editor, window};
use iced_test::{
    Simulator,
    runtime::{Action, task},
};
use ruscello_core::{Download, DownloadId, Source, Status, TorrentSource};
use ruscello_engine::{
    DlcKeyService, EngineEvent, EngineHandle, EngineOptions, TorrentDetailsSnapshot,
    TorrentFilePriority, TorrentFileProgress, TorrentPeerInfo, TorrentPeerState,
    TorrentPeerTransport, TorrentPieceMap, TorrentRuntimeProgress, TorrentSessionTelemetry,
    TorrentSettings, TorrentTrackerInfo, TorrentTrackerStatus,
};
use ruscello_testkit::{FileSpec, TestServer};

use crate::{
    actions::Action as RowAction,
    app::{App, DETAILS_COLLAPSED_HEIGHT, DetailTab, Message},
    desktop::{Receiver, Startup},
    i18n::Strings,
    preferences::{Locale, Preferences},
    settings::{Page, TorrentField},
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
    let start = Instant::now() + Duration::from_secs(60);
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

#[tokio::test]
async fn selects_linkgrabber_targets_and_persists_imported_packages() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let mut app = app(&temp)?;
    app.preferences.check_updates = false;
    let server = TestServer::start().await?;
    let target = server.file(FileSpec::random(7, 64 * 1024).filename("archive.zip"));
    let url = target.url().to_owned();
    let page = server.route("/index", move |_| {
        ruscello_testkit::Reply::html(format!(
            "<title>Archive</title><a download href=\"{url}\">Download archive</a><a href=\"/login\">Sign in</a>"
        ))
    });
    let container = temp.path().join("Sample.dlc");
    tokio::fs::write(
        &container,
        include_bytes!("../../../crates/resolver/tests/fixtures/sample-2026-11-14.dlc"),
    )
    .await?;
    let endpoint = server.route("/dlcrypt/service.php", |_| {
        ruscello_testkit::Reply::html(include_str!(
            "../../../crates/resolver/tests/fixtures/sample-2026-11-14.rc.xml"
        ))
    });
    let mut options =
        EngineOptions::new(temp.path().join("queue.db"), temp.path().join("downloads"));
    options.dlc_service = Some(DlcKeyService::new(
        ruscello_engine::Url::parse(&endpoint)?,
        None,
    ));
    let engine = EngineHandle::start(options).await?;
    apply(&mut app, Message::Started(Some(engine.clone()))).await;
    apply(&mut app, Message::Snapshot(Some(engine.snapshot().await?))).await;
    apply(&mut app, Message::Incoming(vec![page])).await;
    apply(&mut app, Message::Files(vec![container])).await;
    let dialog = app
        .add
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Add dialog was not open"))?;
    assert_eq!(dialog.selected_links.len(), 2);
    assert_eq!(
        dialog.page_candidates().next().map(|page| page.0),
        Some("Archive")
    );
    let download = click(&app, "Download selected")?;
    apply(&mut app, Message::StartImmediately(false)).await;
    apply(&mut app, download).await;
    let snapshot = engine.snapshot().await?;
    assert_eq!(snapshot.downloads.len(), 2);
    assert!(
        snapshot
            .downloads
            .iter()
            .all(|item| item.status() == Status::Paused && item.package.is_some())
    );
    assert!(
        snapshot
            .packages
            .iter()
            .any(|package| package.name == "Test package")
    );
    assert!(
        snapshot
            .downloads
            .iter()
            .any(|item| item.filename.as_deref() == Some("Sample.zip")
                && item.total_bytes == Some(42))
    );
    assert!(
        snapshot
            .packages
            .iter()
            .any(|package| package.name == "Archive")
    );
    engine.shutdown().await?;
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

#[tokio::test]
async fn inspects_selects_files_adds_torrent_and_saves_bittorrent_settings() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let seed_dir = temp.path().join("seed-source").join("bundle");
    std::fs::create_dir_all(&seed_dir)?;
    std::fs::write(seed_dir.join("one.bin"), vec![0x11_u8; 64 * 1024])?;
    std::fs::write(seed_dir.join("two.bin"), vec![0x22_u8; 32 * 1024])?;

    let mut seeder_options =
        EngineOptions::new(temp.path().join("seeder.db"), temp.path().join("seeder-dl"));
    seeder_options.data_dir = Some(temp.path().join("seeder-data"));
    seeder_options.torrent = TorrentSettings::offline_ephemeral();
    let seeder = EngineHandle::start(seeder_options).await?;
    let (torrent_bytes, _, _) = seeder
        .create_and_serve_local_torrent(&seed_dir, Vec::new(), Some(16 * 1024))
        .await?;
    let torrent_path = temp.path().join("bundle.torrent");
    std::fs::write(&torrent_path, &torrent_bytes)?;

    let mut app = app(&temp)?;
    app.preferences.check_updates = false;
    let mut client_options =
        EngineOptions::new(temp.path().join("queue.db"), temp.path().join("downloads"));
    client_options.data_dir = Some(temp.path().join("client-data"));
    client_options.torrent = TorrentSettings::offline_ephemeral();
    let engine = EngineHandle::start(client_options).await?;
    apply(&mut app, Message::Started(Some(engine.clone()))).await;
    apply(&mut app, Message::Snapshot(Some(engine.snapshot().await?))).await;

    let open = click(&app, "Add")?;
    let _ = app.update(open);
    let edit = app.update(Message::EditAdd(text_editor::Action::Edit(
        text_editor::Edit::Paste(std::sync::Arc::new(
            torrent_path.to_string_lossy().into_owned(),
        )),
    )));
    for message in task_messages(edit).await {
        apply(&mut app, message).await;
    }

    assert!(simulator(&app).find("one.bin").is_ok());
    assert!(simulator(&app).find("two.bin").is_ok());

    let none_msg = click(&app, "Select none")?;
    apply(&mut app, none_msg).await;
    assert!(
        !app.add
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing add dialog"))?
            .can_submit()
    );

    let all_msg = click(&app, "Select all")?;
    apply(&mut app, all_msg).await;
    let two_msg = click(&app, "two.bin")?;
    apply(&mut app, two_msg).await;

    let add_msg = click(&app, "Add")?;
    apply(&mut app, add_msg).await;
    let snapshot = engine.snapshot().await?;
    let id = snapshot
        .downloads
        .first()
        .map(|download| download.id)
        .ok_or_else(|| anyhow::anyhow!("Torrent download was not queued"))?;
    apply(&mut app, Message::Snapshot(Some(snapshot))).await;
    apply(&mut app, Message::Select(id)).await;

    let details = engine
        .torrent_details(id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Expected torrent details for queued torrent"))?;
    assert_eq!(details.files.len(), 2);
    let one = details
        .files
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with("one.bin"))
        .ok_or_else(|| anyhow::anyhow!("Expected one.bin in torrent details"))?;
    let two = details
        .files
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with("two.bin"))
        .ok_or_else(|| anyhow::anyhow!("Expected two.bin in torrent details"))?;
    let two_index = two.index;
    assert_eq!(one.priority, TorrentFilePriority::Download);
    assert_eq!(two.priority, TorrentFilePriority::Skip);

    let files_tab = click(&app, "Files")?;
    apply(&mut app, files_tab).await;
    assert!(simulator(&app).find("two.bin").is_ok());
    apply(
        &mut app,
        Message::SetTorrentFilePriority(id, two_index, true),
    )
    .await;
    assert!(app.banner.is_none());
    let updated_details = engine
        .torrent_details(id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Expected updated torrent details"))?;
    let updated_two = updated_details
        .files
        .iter()
        .find(|f| f.index == two_index)
        .ok_or_else(|| anyhow::anyhow!("Expected two.bin in updated torrent details"))?;
    assert_eq!(updated_two.priority, TorrentFilePriority::Download);

    apply(&mut app, Message::Settings(true)).await;
    apply(&mut app, Message::SettingsPage(Page::Torrent)).await;
    assert!(simulator(&app).find("Enable DHT").is_ok());
    apply(
        &mut app,
        Message::TorrentField(TorrentField::Port, "invalid".into()),
    )
    .await;
    let save_bad = click(&app, "Save")?;
    apply(&mut app, save_bad).await;
    assert!(
        simulator(&app)
            .find("Check the numeric settings and try again.")
            .is_ok()
    );
    apply(&mut app, Message::Dismiss).await;
    apply(
        &mut app,
        Message::TorrentField(TorrentField::Port, "0".into()),
    )
    .await;
    apply(
        &mut app,
        Message::TorrentField(TorrentField::MaxPeers, "24".into()),
    )
    .await;
    let save_ok = click(&app, "Save")?;
    apply(&mut app, save_ok).await;
    assert!(app.banner.is_none());

    engine.shutdown().await?;
    seeder.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn displays_torrent_status_telemetry_and_contextual_details_tabs() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let mut app = app(&temp)?;
    app.booting = false;

    let torrent_id = DownloadId(10);
    let mut torrent = Download::new(
        torrent_id,
        Source::Torrent(TorrentSource::Magnet(
            "magnet:?xt=urn:btih:0123456789ABCDEF0123456789ABCDEF01234567&dn=SampleISO".into(),
        )),
        temp.path().to_owned(),
    );
    torrent.filename = Some("SampleISO".into());
    torrent.total_bytes = Some(1_048_576);
    let _ = torrent.transition(Status::Resolving);

    let http_id = DownloadId(11);
    let mut http = Download::new(
        http_id,
        Source::Http {
            url: "https://example.test/archive.zip".into(),
        },
        temp.path().to_owned(),
    );
    http.filename = Some("archive.zip".into());

    app.queue.order.extend([torrent_id, http_id]);
    app.queue.downloads.insert(torrent_id, torrent);
    app.queue.downloads.insert(http_id, http);
    app.queue.rebuild();

    assert!(simulator(&app).find("Fetching metadata").is_ok());

    apply(
        &mut app,
        Message::Event(EngineEvent::TorrentTelemetry(TorrentSessionTelemetry {
            download_speed_bps: 0,
            upload_speed_bps: 4096,
            dht_nodes: Some(42),
            listen_port: Some(6881),
        })),
    )
    .await;
    apply(
        &mut app,
        Message::Event(EngineEvent::TorrentProgress(vec![TorrentRuntimeProgress {
            id: torrent_id,
            download_rate_bps: 1024,
            upload_rate_bps: 4096,
            uploaded_bytes: 8192,
            seed_ratio: 1.5,
            seed_elapsed_secs: 60,
            peers_connected: 3,
            peers_total: 8,
            seeders_connected: 2,
            eta_secs: Some(12),
            piece_count: 16,
        }])),
    )
    .await;
    assert!(simulator(&app).find("DHT: 42 nodes").is_ok());

    apply(&mut app, Message::Select(torrent_id)).await;
    apply(
        &mut app,
        Message::TorrentDetails(
            torrent_id,
            Some(TorrentDetailsSnapshot {
                info_hash: "0123456789abcdef0123456789abcdef01234567".into(),
                is_private: false,
                seed_ratio: 1.5,
                seed_elapsed_secs: 60,
                uploaded_bytes: 8192,
                download_speed_bps: 1024,
                upload_speed_bps: 4096,
                piece_map: Some(TorrentPieceMap {
                    piece_count: 8,
                    piece_length: 131_072,
                    have_count: 4,
                    have_bitmap: vec![0xF0],
                }),
                files: vec![TorrentFileProgress {
                    index: 0,
                    path: std::path::PathBuf::from("sample.iso"),
                    length: 1_048_576,
                    downloaded_bytes: 524_288,
                    priority: TorrentFilePriority::Download,
                }],
                peers: vec![TorrentPeerInfo {
                    address: "198.51.100.7:51413".into(),
                    client: Some("RuscelloTest/1.0".into()),
                    transport: Some(TorrentPeerTransport::Tcp),
                    state: TorrentPeerState::Live,
                    downloaded_bytes: 524_288,
                    uploaded_bytes: 8192,
                    download_speed_bps: 1024,
                    upload_speed_bps: 4096,
                    errors: 0,
                }],
                trackers: vec![TorrentTrackerInfo {
                    url: "http://tracker.local:6969/announce".into(),
                    protocol: "http".into(),
                    status: TorrentTrackerStatus::Announcing,
                }],
            }),
        ),
    )
    .await;

    assert!(simulator(&app).find("Segments").is_err());
    assert!(simulator(&app).find("Pieces").is_ok());
    assert!(simulator(&app).find("Files").is_ok());
    assert!(simulator(&app).find("Peers").is_ok());
    assert!(simulator(&app).find("Trackers").is_ok());

    apply(&mut app, Message::DetailTab(DetailTab::Pieces)).await;
    assert!(simulator(&app).find("4 / 8 pieces (128.0 KiB)").is_ok());
    apply(&mut app, Message::DetailTab(DetailTab::Files)).await;
    assert!(simulator(&app).find("sample.iso").is_ok());
    apply(&mut app, Message::DetailTab(DetailTab::Peers)).await;
    assert!(simulator(&app).find("198.51.100.7:51413").is_ok());
    apply(&mut app, Message::DetailTab(DetailTab::Trackers)).await;
    assert!(
        simulator(&app)
            .find("http://tracker.local:6969/announce")
            .is_ok()
    );

    apply(&mut app, Message::Menu(torrent_id)).await;
    assert!(simulator(&app).find("Force recheck").is_err());

    if let Some(entry) = app.queue.downloads.get_mut(&torrent_id) {
        let _ = entry.transition(Status::Downloading);
    }
    app.queue.rebuild();
    assert!(simulator(&app).find("Force recheck").is_ok());

    apply(&mut app, Message::Select(http_id)).await;
    assert_eq!(app.detail_tab, DetailTab::General);
    assert!(simulator(&app).find("Segments").is_ok());
    assert!(simulator(&app).find("Peers").is_err());
    apply(&mut app, Message::Menu(http_id)).await;
    assert!(simulator(&app).find("Force recheck").is_err());

    Ok(())
}
