use std::{
    collections::BTreeSet,
    error::Error,
    num::NonZeroUsize,
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use aes::cipher::{BlockModeEncrypt as _, KeyIvInit as _, block_padding::NoPadding};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{DateTime, Local, TimeZone, Timelike};
use ruscello_core::{DownloadId, FailureReason, Source, Status};
use ruscello_engine::{
    AddDownload, AutomationSettings, Clock, DlcKeyService, EngineEvent, EngineHandle, EngineLog,
    EngineOptions, Limits, OutsideHoursAction, PostProcessStage, PostProcessingSettings,
    PowerController, QueueCompletionAction, ScheduleAction, ScheduleRule, ScheduleSettings,
    SleepInhibitor, TorrentSettings, Url, WatchFolderRule, WatchFolderSettings,
    WatchProcessedBehavior, WeekdayMask,
};
use ruscello_http::scan_checkpoints;
use ruscello_platform::power::PowerFuture;
use ruscello_testkit::{FileSpec, FtpServerOptions, MockFtpServer, Reply, StatusCode, TestServer};
use sha2::{Digest, Sha256};
use tokio::{sync::Notify, time::timeout};

type TestResult = Result<(), Box<dyn Error>>;

const SERVICE_KEY: &[u8; 16] = b"cb99b5cbc24db398";
const SERVICE_IV: &[u8; 16] = b"9bc24cb995cb8db3";

#[derive(Debug)]
struct MockClock {
    now: Mutex<DateTime<Local>>,
}

impl MockClock {
    fn new(now: DateTime<Local>) -> Self {
        Self {
            now: Mutex::new(now),
        }
    }

    fn set(&self, next: DateTime<Local>) {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) = next;
    }
}

impl Clock for MockClock {
    fn local_now(&self) -> DateTime<Local> {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Debug, Default)]
struct RecordingInhibitor {
    states: Mutex<Vec<bool>>,
}

impl RecordingInhibitor {
    fn states(&self) -> Vec<bool> {
        self.states
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl SleepInhibitor for RecordingInhibitor {
    fn set_inhibited<'a>(&'a self, inhibited: bool, _reason: &'a str) -> PowerFuture<'a, ()> {
        self.states
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(inhibited);
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug, Default)]
struct RecordingPower {
    executed: Mutex<Vec<QueueCompletionAction>>,
    notify: Notify,
}

impl RecordingPower {
    fn executed(&self) -> Vec<QueueCompletionAction> {
        self.executed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl PowerController for RecordingPower {
    fn execute(&self, action: QueueCompletionAction) -> PowerFuture<'_, ()> {
        self.executed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(action);
        self.notify.notify_waiters();
        Box::pin(async { Ok(()) })
    }
}

fn local_datetime(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
) -> Result<DateTime<Local>, Box<dyn Error>> {
    Local
        .with_ymd_and_hms(year, month, day, hour, minute, 0)
        .single()
        .ok_or_else(|| "ambiguous or invalid local test datetime".into())
}

async fn wait_for_status(
    events: &mut tokio::sync::broadcast::Receiver<EngineEvent>,
    target_id: DownloadId,
    expected: Status,
) -> TestResult {
    let mut logs = Vec::new();
    timeout(Duration::from_secs(15), async {
        loop {
            match events.recv().await {
                Ok(EngineEvent::Log { id: Some(id), message }) if id == target_id => {
                    logs.push(format!("{message:?}"));
                }
                Ok(EngineEvent::StatusChanged { id, status }) if id == target_id => {
                    if status == expected {
                        return Ok::<_, Box<dyn Error>>(());
                    }
                    if let Status::Failed { reason } = status
                        && !matches!(expected, Status::Failed { .. })
                    {
                        return Err(format!(
                            "download {target_id:?} failed unexpectedly: {reason:?} (logs: {logs:?})"
                        )
                        .into());
                    }
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(err) => return Err(err.into()),
            }
        }
    })
    .await?
}

fn encrypt_dlc(
    urls: &[&str],
    package_name: &str,
) -> Result<(String, String, String), Box<dyn Error>> {
    fn encrypt(input: &str, key: &[u8; 16], iv: &[u8; 16]) -> Result<String, Box<dyn Error>> {
        let mut bytes = input.as_bytes().to_vec();
        bytes.resize(bytes.len().next_multiple_of(16), 0);
        let length = bytes.len();
        let output = cbc::Encryptor::<aes::Aes128>::new(key.into(), iv.into())
            .encrypt_padded::<NoPadding>(&mut bytes, length)
            .map_err(|_| "AES padding failure")?;
        Ok(STANDARD.encode(output))
    }

    let mut files_xml = String::new();
    for url in urls {
        let encoded_url = STANDARD.encode(url);
        let name = url.rsplit('/').next().unwrap_or("file.bin");
        let encoded_name = STANDARD.encode(name);
        let encoded_size = STANDARD.encode("0");
        files_xml.push_str(&format!(
            "<file><url>{encoded_url}</url><filename>{encoded_name}</filename><size>{encoded_size}</size></file>"
        ));
    }
    let encoded_pkg = STANDARD.encode(package_name);
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><dlc><header><generator><app>UnVzY2VsbG8=</app></generator></header><content><package name=\"{encoded_pkg}\">{files_xml}</package></content></dlc>"
    );
    let plaintext = STANDARD.encode(xml);
    let key = b"test key 16 byte";
    let encrypted = encrypt(&plaintext, key, key)?;
    let tail = STANDARD.encode([3_u8; 64]);
    let rc = encrypt(std::str::from_utf8(key)?, SERVICE_KEY, SERVICE_IV)?;
    Ok((format!("{encrypted}{tail}"), tail, rc))
}

#[tokio::test]
async fn m9_1_scheduler_pauses_outside_window_and_resumes_with_speed_cap() -> TestResult {
    let server = TestServer::start().await?;
    let payload_size = 128 * 1024;
    let file = server.file(
        FileSpec::random(101, payload_size)
            .filename("scheduled.bin")
            .ranges(true)
            .throttle_per_conn(32 * 1024),
    );

    let temp = tempfile::tempdir()?;
    // Monday 2026-06-01 12:00 (outside 22:00..23:30 rule)
    let clock = Arc::new(MockClock::new(local_datetime(2026, 6, 1, 12, 0)?));
    let mut options =
        EngineOptions::new(temp.path().join("queue.db"), temp.path().join("downloads"));
    options.clock = Some(clock.clone());
    options.sleep_inhibitor = Some(Arc::new(RecordingInhibitor::default()));
    options.automation = AutomationSettings {
        prevent_sleep_when_active: false,
        schedule: ScheduleSettings {
            enabled: true,
            outside_hours: OutsideHoursAction::PauseQueue,
            rules: vec![ScheduleRule {
                id: "night-cap".to_owned(),
                name: "Night Cap".to_owned(),
                enabled: true,
                days: WeekdayMask::EVERY_DAY,
                start_minute: 22 * 60,
                end_minute: 23 * 60 + 30,
                action: ScheduleAction::SpeedLimit {
                    max_bytes_per_sec: 1_048_576,
                },
            }],
        },
        ..AutomationSettings::default()
    };

    let engine = EngineHandle::start(options).await?;
    let mut events = engine.subscribe();

    let id = engine
        .add(AddDownload::new(Source::Http {
            url: file.url().into(),
        }))
        .await?;

    // During daytime pause window, scheduler pauses queued/active item.
    timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Ok(EngineEvent::StatusChanged {
                    id: changed,
                    status: Status::Paused,
                }) if changed == id => return Ok::<_, Box<dyn Error>>(()),
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(err) => return Err(err.into()),
            }
        }
    })
    .await??;

    // Advance clock into 22:30 active window -> engine resumes automatically & finishes.
    clock.set(local_datetime(2026, 6, 1, 22, 30)?);
    let mut saw_schedule_resume = false;
    let mut saw_schedule_cap = false;
    timeout(Duration::from_secs(15), async {
        loop {
            match events.recv().await {
                Ok(EngineEvent::Log {
                    id: None,
                    message: EngineLog::ScheduleResumedQueue,
                }) => saw_schedule_resume = true,
                Ok(EngineEvent::ScheduleChanged(decision)) => {
                    if decision.speed_limit_bytes_per_sec == Some(1_048_576) {
                        saw_schedule_cap = true;
                    }
                }
                Ok(EngineEvent::StatusChanged {
                    id: changed,
                    status: Status::Completed,
                }) if changed == id => return Ok::<_, Box<dyn Error>>(()),
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(err) => return Err(err.into()),
            }
        }
    })
    .await??;

    assert!(saw_schedule_resume);
    assert!(saw_schedule_cap);
    assert_eq!(
        std::fs::read(temp.path().join("downloads/scheduled.bin"))?,
        file.payload().bytes(0..payload_size)
    );
    assert_eq!(clock.local_now().hour(), 22);
    engine.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn m9_2_completion_countdown_can_cancel_and_triggers_power_action() -> TestResult {
    let server = TestServer::start().await?;
    let file1 = server.file(FileSpec::random(201, 16 * 1024).filename("first.bin"));
    let file2 = server.file(FileSpec::random(202, 16 * 1024).filename("second.bin"));

    let temp = tempfile::tempdir()?;
    let power = Arc::new(RecordingPower::default());
    let mut options =
        EngineOptions::new(temp.path().join("queue.db"), temp.path().join("downloads"));
    options.sleep_inhibitor = Some(Arc::new(RecordingInhibitor::default()));
    options.power_controller = Some(power.clone());
    options.automation = AutomationSettings {
        prevent_sleep_when_active: false,
        completion_action: QueueCompletionAction::Sleep,
        completion_countdown_secs: 4,
        post_processing: PostProcessingSettings {
            auto_verify_checksums: false,
            ..PostProcessingSettings::default()
        },
        ..AutomationSettings::default()
    };

    let engine = EngineHandle::start(options).await?;
    let mut events = engine.subscribe();

    let id1 = engine
        .add(AddDownload::new(Source::Http {
            url: file1.url().into(),
        }))
        .await?;
    wait_for_status(&mut events, id1, Status::Completed).await?;

    // Wait for cancellable Sleep countdown, then cancel it.
    timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(EngineEvent::CompletionCountdown {
                action: QueueCompletionAction::Sleep,
                ..
            }) = events.recv().await
            {
                return Ok::<_, Box<dyn Error>>(());
            }
        }
    })
    .await??;

    engine.cancel_completion_action().await?;
    timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(EngineEvent::CompletionCountdownCancelled) = events.recv().await {
                return Ok::<_, Box<dyn Error>>(());
            }
        }
    })
    .await??;
    assert!(power.executed().is_empty());

    // Now switch to Shutdown with 1s countdown and verify power action executes cleanly.
    let mut next_settings = engine.snapshot().await?.automation;
    next_settings.completion_action = QueueCompletionAction::Shutdown;
    next_settings.completion_countdown_secs = 1;
    engine.set_automation_settings(next_settings).await?;

    let id2 = engine
        .add(AddDownload::new(Source::Http {
            url: file2.url().into(),
        }))
        .await?;
    wait_for_status(&mut events, id2, Status::Completed).await?;

    timeout(Duration::from_secs(6), async {
        loop {
            if let Ok(EngineEvent::QueueCompleted {
                action: QueueCompletionAction::Shutdown,
            }) = events.recv().await
            {
                return Ok::<_, Box<dyn Error>>(());
            }
        }
    })
    .await??;
    assert_eq!(power.executed(), vec![QueueCompletionAction::Shutdown]);

    engine.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn m9_3_sleep_inhibitor_tracks_active_transfers() -> TestResult {
    let server = TestServer::start().await?;
    let file = server.file(
        FileSpec::random(303, 96 * 1024)
            .filename("inhibit.bin")
            .ranges(true)
            .throttle_per_conn(32 * 1024),
    );

    let temp = tempfile::tempdir()?;
    let inhibitor = Arc::new(RecordingInhibitor::default());
    let mut options =
        EngineOptions::new(temp.path().join("queue.db"), temp.path().join("downloads"));
    options.sleep_inhibitor = Some(inhibitor.clone());
    options.automation = AutomationSettings {
        prevent_sleep_when_active: true,
        ..AutomationSettings::default()
    };

    let engine = EngineHandle::start(options).await?;
    let mut events = engine.subscribe();
    let id = engine
        .add(AddDownload::new(Source::Http {
            url: file.url().into(),
        }))
        .await?;

    wait_for_status(&mut events, id, Status::Completed).await?;
    // Allow one automation tick after completion so inhibition turns back off.
    timeout(Duration::from_secs(4), async {
        loop {
            let states = inhibitor.states();
            if states.contains(&true) && states.last().copied() == Some(false) {
                return Ok::<_, Box<dyn Error>>(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await??;

    let recorded = inhibitor.states();
    assert!(recorded.contains(&true));
    assert_eq!(recorded.last().copied(), Some(false));
    engine.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn m9_4_5_7_verifies_checksum_waits_for_multipart_7z_and_runs_hook() -> TestResult {
    let temp = tempfile::tempdir()?;
    // Create encrypted .7z archive containing "hello.txt" using compress feature
    let src_dir = temp.path().join("src");
    std::fs::create_dir_all(&src_dir)?;
    std::fs::write(src_dir.join("hello.txt"), b"encrypted-multipart-payload")?;
    let mut full_archive_cursor = std::io::Cursor::new(Vec::new());
    sevenz_rust2::compress_encrypted(
        &src_dir,
        &mut full_archive_cursor,
        sevenz_rust2::Password::from("hunter2"),
    )?;
    let full_archive = full_archive_cursor.into_inner();
    let split_at = full_archive.len() / 2;
    let part1 = full_archive[..split_at].to_vec();
    let part2 = full_archive[split_at..].to_vec();

    let part1_digest = hex::encode(Sha256::digest(&part1));
    let part2_digest = hex::encode(Sha256::digest(&part2));
    let sha_manifest =
        format!("{part1_digest} *bundle.7z.001\n{part2_digest} *bundle.7z.002\n").into_bytes();

    // Also prepare a corrupted payload that violates a .sha256 sidecar.
    let bad_payload = b"actually-corrupted-bytes".to_vec();
    let good_digest = hex::encode(Sha256::digest(b"expected-clean-bytes"));
    let bad_manifest = format!("{good_digest} *corrupt.bin\n").into_bytes();

    let server = TestServer::start().await?;
    let part1_url = {
        let p = part1.clone();
        server.route("/files/bundle.7z.001", move |_| {
            Reply::bytes("application/octet-stream", p.clone())
        })
    };
    let part2_url = {
        let p = part2.clone();
        server.route("/files/bundle.7z.002", move |_| {
            Reply::bytes("application/octet-stream", p.clone())
        })
    };
    let sha_url = {
        let s = sha_manifest.clone();
        server.route("/files/bundle.sha256", move |_| {
            Reply::bytes("text/plain", s.clone())
        })
    };
    let bad_url = server.route("/files/corrupt.bin", move |_| {
        Reply::bytes("application/octet-stream", bad_payload.clone())
    });
    let bad_sha_url = server.route("/files/corrupt.sha256", move |_| {
        Reply::bytes("text/plain", bad_manifest.clone())
    });

    #[cfg(unix)]
    let hook_cmd = Some(
        "/bin/sh -c 'printf \"%s|%s|%s\" \"$1\" \"$RUSCELLO_FILE\" \"$RUSCELLO_EXTRACTED_DIR\" > \"$2\"' _ {name} {dir}/hook.txt"
            .to_owned(),
    );
    #[cfg(not(unix))]
    let hook_cmd = None;

    let mut options =
        EngineOptions::new(temp.path().join("queue.db"), temp.path().join("downloads"));
    options.sleep_inhibitor = Some(Arc::new(RecordingInhibitor::default()));
    options.limits = Limits {
        concurrent_downloads: NonZeroUsize::MIN.saturating_add(3),
        ..Limits::default()
    };
    options.automation = AutomationSettings {
        prevent_sleep_when_active: false,
        post_processing: PostProcessingSettings {
            auto_verify_checksums: true,
            auto_extract: true,
            extract_into_subfolder: true,
            delete_archives_after_extraction: true,
            archive_passwords: vec!["wrong".to_owned(), "hunter2".to_owned()],
            external_extractor_path: None,
            post_download_command: hook_cmd,
            post_download_timeout_secs: 15,
        },
        ..AutomationSettings::default()
    };

    let engine = EngineHandle::start(options).await?;
    let mut events = engine.subscribe();

    // First queue checksum manifest alone so it lands before archive parts finish.
    let sha_id = engine
        .add_package(
            "Bundle".to_owned(),
            vec![AddDownload::new(Source::Http { url: sha_url })],
        )
        .await?[0];
    wait_for_status(&mut events, sha_id, Status::Completed).await?;

    // Now queue part 2 PAUSED + part 1 ACTIVE in the same package -> barrier must hold!
    let mut req2 = AddDownload::new(Source::Http { url: part2_url });
    req2.start_immediately = false;
    let part_ids = engine
        .add_package(
            "Bundle".to_owned(),
            vec![AddDownload::new(Source::Http { url: part1_url }), req2],
        )
        .await?;
    let part1_id = part_ids[0];
    let part2_id = part_ids[1];
    wait_for_status(&mut events, part1_id, Status::Completed).await?;
    assert!(
        !temp.path().join("downloads/bundle/hello.txt").exists(),
        "archive must not extract before part 2 finishes"
    );

    // Resume part 2 -> verifies checksums, extracts split 7z with password, deletes archives, runs hook.
    engine.resume(part2_id).await?;
    let mut stages = Vec::new();
    let mut saw_checksum_ok = false;
    let mut saw_extracted = false;
    let mut saw_hook_ok = false;
    timeout(Duration::from_secs(15), async {
        loop {
            match events.recv().await {
                Ok(EngineEvent::PostProcessing { id, stage }) if id == part2_id => {
                    stages.push(stage);
                }
                Ok(EngineEvent::Log {
                    id: Some(id),
                    message: EngineLog::ChecksumVerified,
                }) if id == part2_id => saw_checksum_ok = true,
                Ok(EngineEvent::Log {
                    id: Some(id),
                    message: EngineLog::ArchiveExtracted,
                }) if id == part2_id => saw_extracted = true,
                Ok(EngineEvent::Log {
                    id: Some(id),
                    message: EngineLog::PostProcessCommandExecuted,
                }) if id == part2_id => saw_hook_ok = true,
                Ok(EngineEvent::StatusChanged {
                    id,
                    status: Status::Completed,
                }) if id == part2_id => return Ok::<_, Box<dyn Error>>(()),
                Ok(EngineEvent::StatusChanged {
                    id,
                    status: Status::Failed { reason },
                }) if id == part2_id => {
                    return Err(format!("part2 failed: {reason:?}").into());
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(err) => return Err(err.into()),
            }
        }
    })
    .await??;

    assert!(saw_checksum_ok);
    assert!(saw_extracted);
    assert!(stages.contains(&PostProcessStage::VerifyingChecksum));
    assert!(stages.contains(&PostProcessStage::ExtractingArchive));
    assert_eq!(
        std::fs::read(temp.path().join("downloads/bundle/hello.txt"))?,
        b"encrypted-multipart-payload"
    );
    assert!(!temp.path().join("downloads/bundle.7z.001").exists());
    assert!(!temp.path().join("downloads/bundle.7z.002").exists());

    #[cfg(unix)]
    {
        assert!(saw_hook_ok);
        assert!(stages.contains(&PostProcessStage::RunningCommand));
        let hook_out = std::fs::read_to_string(temp.path().join("downloads/hook.txt"))?;
        assert!(hook_out.starts_with("bundle.7z.002|"));
        assert!(hook_out.contains("bundle"));
    }
    #[cfg(not(unix))]
    let _ = saw_hook_ok;

    // Verify corrupted download fails sidecar integrity check cleanly.
    let bad_sha_id = engine
        .add(AddDownload::new(Source::Http { url: bad_sha_url }))
        .await?;
    wait_for_status(&mut events, bad_sha_id, Status::Completed).await?;
    let bad_id = engine
        .add(AddDownload::new(Source::Http { url: bad_url }))
        .await?;
    wait_for_status(
        &mut events,
        bad_id,
        Status::Failed {
            reason: FailureReason::IntegrityCheck,
        },
    )
    .await?;

    engine.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn m9_6_watch_folders_import_torrent_dlc_and_txt() -> TestResult {
    let temp = tempfile::tempdir()?;
    let server = TestServer::start().await?;
    let txt_file = server.file(FileSpec::random(601, 8 * 1024).filename("from-txt.bin"));
    let dlc_target = server.file(FileSpec::random(602, 8 * 1024).filename("from-dlc.bin"));

    let dlc_url = dlc_target.url().to_owned();
    let (dlc_raw, tail, rc) = encrypt_dlc(&[&dlc_url], "DlcBatch")?;
    let key_endpoint = server.route("/dlc-key", move |request| {
        let expected = format!(
            "srcType=dlc&destType=pylo&data={}",
            tail.replace('=', "%3D")
        );
        if request.uri.query() == Some(expected.as_str()) {
            Reply::html(format!("<response><rc>{rc}</rc></response>"))
        } else {
            Reply::status(StatusCode::BAD_REQUEST)
        }
    });

    // Seed a local torrent via a second ephemeral engine + loopback HTTP announce endpoint.
    let seeder_addr_slot: Arc<std::sync::OnceLock<std::net::SocketAddr>> =
        Arc::new(std::sync::OnceLock::new());
    let announce_url = {
        let slot = Arc::clone(&seeder_addr_slot);
        server.route("/announce", move |_| {
            let Some(addr) = slot.get().copied() else {
                return Reply::bytes("text/plain", b"d14:failure reason9:not ready".to_vec());
            };
            let ip_octets = match addr.ip() {
                std::net::IpAddr::V4(v4) => v4.octets(),
                std::net::IpAddr::V6(_) => [127, 0, 0, 1],
            };
            let mut body = b"d8:intervali60e5:peers6:".to_vec();
            body.extend_from_slice(&ip_octets);
            body.extend_from_slice(&addr.port().to_be_bytes());
            body.push(b'e');
            Reply::bytes("text/plain", body)
        })
    };
    let seeder_dir = tempfile::tempdir()?;
    let seed_src = seeder_dir.path().join("seed-src");
    std::fs::create_dir_all(&seed_src)?;
    let torrent_payload = b"watch-folder-torrent-data".repeat(256);
    std::fs::write(seed_src.join("watched.dat"), &torrent_payload)?;
    let mut seeder_opts = EngineOptions::new(
        seeder_dir.path().join("queue.db"),
        seeder_dir.path().join("downloads"),
    );
    seeder_opts.data_dir = Some(seeder_dir.path().join("data"));
    seeder_opts.torrent = TorrentSettings::offline_ephemeral();
    seeder_opts.eager_torrent_session = true;
    seeder_opts.sleep_inhibitor = Some(Arc::new(RecordingInhibitor::default()));
    let seeder = EngineHandle::start(seeder_opts).await?;
    let (torrent_bytes, _hash, seeder_addr) = seeder
        .create_and_serve_local_torrent(&seed_src, vec![announce_url], Some(16_384))
        .await?;
    let _ = seeder_addr_slot.set(seeder_addr);

    let watch_move = temp.path().join("watch-move");
    let watch_delete = temp.path().join("watch-delete");
    let watch_keep = temp.path().join("watch-keep");
    std::fs::create_dir_all(&watch_move)?;
    std::fs::create_dir_all(&watch_delete)?;
    std::fs::create_dir_all(&watch_keep)?;

    std::fs::write(
        watch_move.join("links.txt"),
        format!("{}\n", txt_file.url()),
    )?;
    std::fs::write(watch_delete.join("pack.dlc"), &dlc_raw)?;
    std::fs::write(watch_keep.join("watched.torrent"), &torrent_bytes)?;

    let mut options =
        EngineOptions::new(temp.path().join("queue.db"), temp.path().join("downloads"));
    options.data_dir = Some(temp.path().join("data"));
    options.torrent = TorrentSettings::offline_ephemeral();
    options.eager_torrent_session = true;
    options.sleep_inhibitor = Some(Arc::new(RecordingInhibitor::default()));
    options.dlc_service = Some(DlcKeyService::new(Url::parse(&key_endpoint)?, None));
    options.automation = AutomationSettings {
        prevent_sleep_when_active: false,
        watch_folders: WatchFolderSettings {
            enabled: true,
            poll_interval_secs: 30,
            rules: vec![
                WatchFolderRule {
                    id: "w-txt".to_owned(),
                    enabled: true,
                    directory: watch_move.clone(),
                    import_torrents: false,
                    import_dlc: false,
                    import_txt: true,
                    category: None,
                    destination: None,
                    processed_behavior: WatchProcessedBehavior::MoveToProcessed,
                },
                WatchFolderRule {
                    id: "w-dlc".to_owned(),
                    enabled: true,
                    directory: watch_delete.clone(),
                    import_torrents: false,
                    import_dlc: true,
                    import_txt: false,
                    category: None,
                    destination: None,
                    processed_behavior: WatchProcessedBehavior::Delete,
                },
                WatchFolderRule {
                    id: "w-torrent".to_owned(),
                    enabled: true,
                    directory: watch_keep.clone(),
                    import_torrents: true,
                    import_dlc: false,
                    import_txt: false,
                    category: None,
                    destination: None,
                    processed_behavior: WatchProcessedBehavior::KeepTracked,
                },
            ],
        },
        ..AutomationSettings::default()
    };

    let engine = EngineHandle::start(options).await?;
    let mut events = engine.subscribe();
    engine.scan_watch_folders_now().await?;

    let mut completed = BTreeSet::new();
    match timeout(Duration::from_secs(12), async {
        loop {
            match events.recv().await {
                Ok(EngineEvent::StatusChanged { id, status }) => {
                    if let Status::Failed { reason } = status {
                        return Err(format!("watched download {id:?} failed: {reason:?}").into());
                    }
                    if matches!(status, Status::Completed | Status::Seeding) {
                        completed.insert(id);
                    }
                    if completed.len() >= 3
                        && temp.path().join("downloads/from-txt.bin").exists()
                        && temp.path().join("downloads/from-dlc.bin").exists()
                        && (temp.path().join("downloads/watched.dat").exists()
                            || temp.path().join("downloads/seed-src/watched.dat").exists()
                            || temp.path().join("downloads/watched/watched.dat").exists())
                    {
                        return Ok::<_, Box<dyn Error>>(());
                    }
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(err) => return Err(err.into()),
            }
        }
    })
    .await
    {
        Ok(res) => res?,
        Err(_) => {
            let snap = engine.snapshot().await?;
            let summary: Vec<_> = snap
                .downloads
                .iter()
                .map(|d| format!("{:?}:{:?}:{:?}", d.id, d.filename, d.status()))
                .collect();
            return Err(format!(
                "timed out waiting for watch imports; completed={completed:?}; downloads={summary:?}"
            )
            .into());
        }
    }

    assert!(!watch_move.join("links.txt").exists());
    assert!(watch_move.join("processed/links.txt").exists());
    assert!(!watch_delete.join("pack.dlc").exists());
    assert!(watch_keep.join("watched.torrent").exists());

    // Triggering another scan with KeepTracked must not duplicate the torrent import.
    let count_before = engine.snapshot().await?.downloads.len();
    engine.scan_watch_folders_now().await?;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(engine.snapshot().await?.downloads.len(), count_before);

    engine.shutdown().await?;
    seeder.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn m9_8_ftps_download_resumes_and_redacts_password_from_persistence() -> TestResult {
    let ftp = MockFtpServer::start(FtpServerOptions {
        credentials: Some(("alice".to_owned(), "super-secret-pw".to_owned())),
        allow_tls: true,
        require_tls: true,
        drop_first_after_bytes: Some(8192),
        allow_restart: true,
        support_size: true,
    })
    .await;
    let payload: Vec<u8> = (0..(48 * 1024_usize))
        .map(|i| ((i * 19) % 251) as u8)
        .collect();
    ftp.register_file("/pub/secure.bin", payload.clone());

    let temp = tempfile::tempdir()?;
    let db_path: PathBuf = temp.path().join("queue.db");
    let mut options = EngineOptions::new(db_path.clone(), temp.path().join("downloads"));
    options.sleep_inhibitor = Some(Arc::new(RecordingInhibitor::default()));
    options.ftp_root_certificates = vec![ftp.ca_cert_der().to_vec()];
    options.automation.post_processing.auto_verify_checksums = false;

    let engine = EngineHandle::start(options.clone()).await?;
    let mut events = engine.subscribe();
    let id = engine
        .add(AddDownload::new(Source::Ftp {
            url: format!(
                "ftps://alice:super-secret-pw@127.0.0.1:{}/pub/secure.bin",
                ftp.port()
            ),
        }))
        .await?;

    wait_for_status(&mut events, id, Status::Completed).await?;
    assert_eq!(
        std::fs::read(temp.path().join("downloads/secure.bin"))?,
        payload
    );
    assert!(scan_checkpoints(&temp.path().join("downloads"))?.is_empty());

    let commands = ftp.observed_commands();
    assert!(commands.iter().any(|cmd| cmd == "AUTH TLS"));
    assert!(commands.iter().any(|cmd| cmd == "PROT P"));
    assert!(commands.iter().any(|cmd| cmd == "REST 8192"));

    let snapshot = engine.snapshot().await?;
    assert!(matches!(
        &snapshot.downloads[0].source,
        Source::Ftp { url } if !url.contains("super-secret-pw") && url.starts_with("ftps://alice@")
    ));
    engine.shutdown().await?;

    let db_bytes = std::fs::read(&db_path)?;
    assert!(
        !db_bytes
            .windows(b"super-secret-pw".len())
            .any(|w| w == b"super-secret-pw"),
        "FTP password leaked into SQLite database!"
    );

    // Restart engine from disk to verify persisted Source::Ftp reloads cleanly.
    let engine = EngineHandle::start(options).await?;
    let snapshot = engine.snapshot().await?;
    assert_eq!(snapshot.downloads.len(), 1);
    assert_eq!(snapshot.downloads[0].status(), Status::Completed);
    engine.shutdown().await?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn m9_4_rar_extracts_using_external_tool_stub() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let script_path = temp.path().join("mock-unrar.sh");
    let script = "#!/bin/sh\nset -e\nout_dir=\"\"\nlast_arg=\"\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    -o+|-y|-p*) ;;\n    -o*) out_dir=\"${arg#-o}\" ;;\n    *) last_arg=\"$arg\" ;;\n  esac\ndone\nif [ -z \"$out_dir\" ]; then\n  out_dir=\"$last_arg\"\nfi\nmkdir -p \"$out_dir\"\nprintf 'unrar-engine-ok' > \"$out_dir/unpacked.txt\"\n";
    std::fs::write(&script_path, script)?;
    let mut perms = std::fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script_path, perms)?;

    let server = TestServer::start().await?;
    let rar_url = server.route("/release.rar", |_| {
        Reply::bytes("application/vnd.rar", b"Rar!\x1a\x07\x01\x00dummy".to_vec())
    });

    let mut options =
        EngineOptions::new(temp.path().join("queue.db"), temp.path().join("downloads"));
    options.sleep_inhibitor = Some(Arc::new(RecordingInhibitor::default()));
    options.automation.post_processing = PostProcessingSettings {
        auto_verify_checksums: false,
        auto_extract: true,
        extract_into_subfolder: true,
        delete_archives_after_extraction: false,
        external_extractor_path: Some(script_path),
        ..PostProcessingSettings::default()
    };

    let engine = EngineHandle::start(options).await?;
    let mut events = engine.subscribe();
    let id = engine
        .add(AddDownload::new(Source::Http { url: rar_url }))
        .await?;
    wait_for_status(&mut events, id, Status::Completed).await?;
    assert_eq!(
        std::fs::read_to_string(temp.path().join("downloads/release/unpacked.txt"))?,
        "unrar-engine-ok"
    );
    engine.shutdown().await?;
    Ok(())
}
