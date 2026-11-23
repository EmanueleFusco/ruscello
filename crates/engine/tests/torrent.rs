use std::{
    error::Error,
    fmt,
    num::NonZeroU64,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use ruscello_core::{
    DefaultCategory, Download, DownloadId, FileType, Source, SourceKind, Status, TorrentSource,
};
use ruscello_engine::{
    AddDownload, CategoryMatch, CategoryRule, EngineError, EngineEvent, EngineHandle,
    EngineOptions, ScheduleAction, ScheduleRule, ScheduleSettings, TorrentError,
    TorrentFilePriority, TorrentSettings,
};
use ruscello_testkit::{FileSpec, TestServer};
use tempfile::TempDir;
use tokio::{sync::broadcast, time::timeout};
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn Error>>;

struct CapturedFieldVisitor(String);

impl tracing::field::Visit for CapturedFieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if !self.0.is_empty() {
            self.0.push_str(", ");
        }
        self.0.push_str(&format!("{}={value:?}", field.name()));
    }
}

struct WarnCapture(Arc<Mutex<Vec<String>>>);

impl tracing::Subscriber for WarnCapture {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() <= tracing::Level::WARN
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut visitor = CapturedFieldVisitor(String::new());
        event.record(&mut visitor);
        let entry = format!(
            "[{}] {}: {}",
            event.metadata().level(),
            event.metadata().target(),
            visitor.0
        );
        if let Ok(mut guard) = self.0.lock() {
            guard.push(entry);
        }
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

fn captured_warn_logs() -> &'static Mutex<Vec<String>> {
    static LOGS: OnceLock<Arc<Mutex<Vec<String>>>> = OnceLock::new();
    LOGS.get_or_init(|| {
        let store = Arc::new(Mutex::new(Vec::new()));
        let _ = tracing::subscriber::set_global_default(WarnCapture(Arc::clone(&store)));
        store
    })
}

fn recent_warn_logs() -> String {
    captured_warn_logs()
        .lock()
        .map(|g| {
            g.iter()
                .rev()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .unwrap_or_default()
}

fn deterministic_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let word = state.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes();
        let take = (len - out.len()).min(word.len());
        out.extend_from_slice(&word[..take]);
    }
    out
}

async fn wait_for_status(
    engine: &EngineHandle,
    events: &mut broadcast::Receiver<EngineEvent>,
    id: DownloadId,
    expected: Status,
    budget: Duration,
) -> Result<(), Box<dyn Error>> {
    let _ = captured_warn_logs();
    let check_snapshot =
        |snapshot: ruscello_engine::EngineSnapshot| -> Result<bool, Box<dyn Error>> {
            if let Some(download) = snapshot.downloads.iter().find(|d| d.id == id) {
                let status = download.status();
                if status == expected {
                    return Ok(true);
                }
                if !matches!(expected, Status::Failed { .. })
                    && let Status::Failed { reason } = status
                {
                    return Err(format!(
                        "download {} unexpectedly failed ({reason:?}) while waiting for {expected:?}; warn logs: {}",
                        id.0,
                        recent_warn_logs()
                    )
                    .into());
                }
            }
            Ok(false)
        };

    if check_snapshot(engine.snapshot().await?)? {
        return Ok(());
    }
    let mut poll = tokio::time::interval(Duration::from_millis(200));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    timeout(budget, async {
        loop {
            tokio::select! {
                recv = events.recv() => match recv {
                    Ok(EngineEvent::StatusChanged { id: actual, status }) if actual == id => {
                        if status == expected {
                            return Ok(());
                        }
                        if !matches!(expected, Status::Failed { .. })
                            && let Status::Failed { reason } = status
                        {
                            return Err(format!(
                                "download {} unexpectedly failed ({reason:?}) while waiting for {expected:?}; warn logs: {}",
                                id.0,
                                recent_warn_logs()
                            )
                            .into());
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if check_snapshot(engine.snapshot().await?)? {
                            return Ok(());
                        }
                    }
                    Err(err) => return Err(Box::new(err) as Box<dyn Error>),
                },
                _ = poll.tick() => {
                    if check_snapshot(engine.snapshot().await?)? {
                        return Ok(());
                    }
                }
            }
        }
    })
    .await?
}

fn find_download(downloads: &[Download], id: DownloadId) -> Result<&Download, Box<dyn Error>> {
    downloads
        .iter()
        .find(|d| d.id == id)
        .ok_or_else(|| format!("missing download {}", id.0).into())
}

fn torrent_options(dir: &Path) -> EngineOptions {
    let mut opts = EngineOptions::new(dir.join("queue.db"), dir.join("downloads"));
    opts.data_dir = Some(dir.join("data"));
    opts.torrent = TorrentSettings::offline_ephemeral();
    opts.eager_torrent_session = true;
    opts
}

#[tokio::test]
async fn existing_torrent_payload_finishes_checking_and_verifies_nested_manifest() -> TestResult {
    use sha2::{Digest as _, Sha256};
    let root = TempDir::new()?;
    let options = torrent_options(root.path());
    let source = options.destination.join("already-present");
    std::fs::create_dir_all(source.join("sums"))?;
    let payload = deterministic_bytes(46, 512 * 1024);
    let digest = hex::encode(Sha256::digest(&payload));
    std::fs::write(source.join("payload.bin"), &payload)?;
    std::fs::write(
        source.join("sums/files.sha256"),
        format!("{digest} *..\\payload.bin\n"),
    )?;
    let engine = EngineHandle::start(options).await?;
    let seed_path = root.path().join("fixture-seeder");
    std::fs::create_dir_all(&seed_path)?;
    let owner = EngineHandle::start(torrent_options(&seed_path)).await?;
    let (metainfo, _, _) = owner
        .create_and_serve_local_torrent(&source, vec![], None)
        .await?;
    owner.shutdown().await?;
    let mut events = engine.subscribe();
    let mut verification = engine.subscribe();
    let mut request = AddDownload::new(Source::Torrent(TorrentSource::File(
        root.path().join("local.torrent"),
    )));
    request.cached_torrent_bytes = Some(metainfo);
    let id = engine.add(request).await?;
    wait_for_status(
        &engine,
        &mut events,
        id,
        Status::Seeding,
        Duration::from_secs(10),
    )
    .await?;
    timeout(Duration::from_secs(3), async {
        loop {
            if matches!(verification.recv().await?, EngineEvent::Log { id: Some(actual), message: ruscello_engine::EngineLog::ChecksumVerified } if actual == id) {
                return Ok::<_, broadcast::error::RecvError>(());
            }
        }
    }).await??;
    assert_eq!(std::fs::read(source.join("payload.bin"))?, payload);
    engine.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn http_and_torrent_use_one_live_global_download_budget() -> TestResult {
    let root = TempDir::new()?;
    let seed_dir = root.path().join("seed");
    std::fs::create_dir(&seed_dir)?;
    let seed = EngineHandle::start(torrent_options(&seed_dir)).await?;
    let payload = deterministic_bytes(622, 4 * 1024 * 1024);
    let source = seed_dir.join("torrent.bin");
    std::fs::write(&source, &payload)?;
    let (metainfo, _, peer) = seed
        .create_and_serve_local_torrent(&source, vec![], Some(16_384))
        .await?;
    let server = TestServer::start().await?;
    let file = server.file(FileSpec::random(287, 4 * 1024 * 1024).filename("http.bin"));
    let mut options = torrent_options(&root.path().join("client"));
    let scheduled_cap = 384 * 1024;
    options.automation.schedule = ScheduleSettings {
        enabled: true,
        rules: vec![ScheduleRule {
            start_minute: 0,
            end_minute: 0,
            action: ScheduleAction::SpeedLimit {
                max_bytes_per_sec: scheduled_cap,
            },
            ..ScheduleRule::default()
        }],
        ..ScheduleSettings::default()
    };
    std::fs::create_dir_all(&options.destination)?;
    let destination = options.destination.clone();
    let client = EngineHandle::start(options).await?;

    let mut torrent = AddDownload::new(Source::Torrent(TorrentSource::File(
        root.path().join("local.torrent"),
    )));
    torrent.cached_torrent_bytes = Some(metainfo);
    torrent.initial_peers = vec![peer];
    let torrent_id = client.add(torrent).await?;
    let http_id = client
        .add(AddDownload::new(Source::Http {
            url: file.url().to_owned(),
        }))
        .await?;
    let mut events = client.subscribe();
    wait_for_status(
        &client,
        &mut events,
        torrent_id,
        Status::Downloading,
        Duration::from_secs(10),
    )
    .await?;
    wait_for_status(
        &client,
        &mut events,
        http_id,
        Status::Downloading,
        Duration::from_secs(10),
    )
    .await?;
    let ids = [torrent_id, http_id];
    assert_shared_payload_budget(&client, ids, scheduled_cap).await?;
    let temporary_cap = 512 * 1024;
    client
        .set_temporary_bandwidth(NonZeroU64::new(temporary_cap))
        .await?;
    let temporary_rate = assert_shared_payload_budget(&client, ids, temporary_cap).await?;
    assert!(
        temporary_rate > scheduled_cap as f64 * 1.05,
        "the temporary cap did not supersede the scheduled limit"
    );
    client.set_temporary_bandwidth(None).await?;
    assert_shared_payload_budget(&client, ids, scheduled_cap).await?;
    client.set_automation_settings(Default::default()).await?;
    wait_for_status(
        &client,
        &mut events,
        torrent_id,
        Status::Seeding,
        Duration::from_secs(10),
    )
    .await?;
    wait_for_status(
        &client,
        &mut events,
        http_id,
        Status::Completed,
        Duration::from_secs(10),
    )
    .await?;
    assert_eq!(std::fs::read(destination.join("torrent.bin"))?, payload);
    assert_eq!(
        std::fs::read(destination.join("http.bin"))?,
        file.payload().bytes(0..file.payload().len())
    );
    client.shutdown().await?;
    seed.shutdown().await?;
    Ok(())
}

async fn assert_shared_payload_budget(
    client: &EngineHandle,
    ids: [DownloadId; 2],
    cap: u64,
) -> Result<f64, Box<dyn Error>> {
    let before = client.snapshot().await?;
    let start = std::time::Instant::now();
    tokio::time::sleep(Duration::from_secs(4)).await;
    let after = client.snapshot().await?;
    let mut combined = 0;
    for id in ids {
        let updated = find_download(&after.downloads, id)?;
        let delta = updated
            .downloaded_bytes
            .saturating_sub(find_download(&before.downloads, id)?.downloaded_bytes);
        assert!(
            delta > 0,
            "{:?} made no progress while sharing the {cap} B/s cap (status: {:?}, downloaded: {})",
            updated.source.kind(),
            updated.status(),
            updated.downloaded_bytes
        );
        combined += delta;
    }
    let observed_rate = combined as f64 / start.elapsed().as_secs_f64();
    assert!(
        observed_rate < cap as f64 * 1.15,
        "combined rate {observed_rate:.0} B/s exceeded the shared cap {cap}"
    );
    eprintln!("Shared cap {cap} B/s, observed combined payload {observed_rate:.0} B/s");
    Ok(observed_rate)
}

#[tokio::test]
async fn inspects_selects_files_updates_settings_and_completes_on_seed_ratio() -> TestResult {
    let seeder_dir = TempDir::new()?;
    let src_dir = seeder_dir.path().join("release");
    std::fs::create_dir_all(&src_dir)?;
    let main_bytes = deterministic_bytes(11, 16_384 * 6);
    let extra_bytes = deterministic_bytes(22, 16_384 * 4);
    std::fs::write(src_dir.join("main.bin"), &main_bytes)?;
    std::fs::write(src_dir.join("extra.bin"), &extra_bytes)?;

    let seeder = EngineHandle::start(torrent_options(seeder_dir.path())).await?;
    let (torrent_bytes, info_hash, seeder_addr) = seeder
        .create_and_serve_local_torrent(&src_dir, Vec::new(), Some(16_384))
        .await?;

    let leecher_dir = TempDir::new()?;
    let leecher_dest = leecher_dir.path().join("downloads");
    let torrent_file = leecher_dir.path().join("release.torrent");
    std::fs::write(&torrent_file, &torrent_bytes)?;

    let leecher = EngineHandle::start(torrent_options(leecher_dir.path())).await?;
    let mut events = leecher.subscribe();

    let cancel = CancellationToken::new();
    let inspected = leecher
        .inspect_torrent(
            TorrentSource::File(torrent_file.clone()),
            Vec::new(),
            cancel,
        )
        .await?;
    assert_eq!(inspected.info_hash, info_hash);
    assert_eq!(inspected.files.len(), 2);

    let main_idx = inspected
        .files
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with("main.bin"))
        .ok_or("missing main.bin")?
        .index;
    let extra_idx = inspected
        .files
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with("extra.bin"))
        .ok_or("missing extra.bin")?
        .index;

    let mut req = AddDownload::new(Source::Torrent(TorrentSource::File(torrent_file)));
    req.only_files = Some(vec![main_idx]);
    req.cached_torrent_bytes = inspected.torrent_bytes.clone();
    req.initial_peers = vec![seeder_addr];
    req.filename_hint = Some(inspected.name.clone());

    let id = leecher.add(req).await?;
    wait_for_status(
        &leecher,
        &mut events,
        id,
        Status::Seeding,
        Duration::from_secs(20),
    )
    .await?;

    let snap = leecher.snapshot().await?;
    let entry = find_download(&snap.downloads, id)?;
    assert_eq!(entry.file_type, Some(FileType::Torrents));
    assert_eq!(
        std::fs::read(leecher_dest.join(&inspected.name).join("main.bin"))?,
        main_bytes
    );

    let details = leecher
        .torrent_details(id)
        .await?
        .ok_or("expected torrent details while seeding")?;
    assert_eq!(details.info_hash, info_hash);
    assert_eq!(
        details
            .files
            .iter()
            .find(|f| f.index == extra_idx)
            .ok_or("missing extra file in details")?
            .priority,
        TorrentFilePriority::Skip
    );
    assert!(details.piece_map.is_some());

    // Dynamically include the previously skipped file while active, then complete via global seed limit.
    leecher
        .set_torrent_files(id, vec![main_idx, extra_idx])
        .await?;
    timeout(Duration::from_secs(20), async {
        loop {
            if std::fs::read(leecher_dest.join(&inspected.name).join("extra.bin"))
                .is_ok_and(|bytes| bytes == extra_bytes)
            {
                return Ok::<_, Box<dyn Error>>(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await??;

    // Wait until it settles back to Seeding after fetching extra.bin, then apply seed_ratio_millis = 0.
    wait_for_status(
        &leecher,
        &mut events,
        id,
        Status::Seeding,
        Duration::from_secs(10),
    )
    .await?;

    let updated = TorrentSettings {
        seed_ratio_millis: Some(0),
        enable_utp: true,
        max_global_peers: std::num::NonZeroUsize::MIN.saturating_add(39),
        ..TorrentSettings::offline_ephemeral()
    };
    leecher.set_torrent_settings(updated.clone()).await?;
    wait_for_status(
        &leecher,
        &mut events,
        id,
        Status::Completed,
        Duration::from_secs(10),
    )
    .await?;

    let final_snap = leecher.snapshot().await?;
    assert_eq!(final_snap.torrent, updated);
    assert_eq!(
        find_download(&final_snap.downloads, id)?.status(),
        Status::Completed
    );

    // Invalid bind interface is rejected cleanly without losing previous valid settings.
    let invalid = TorrentSettings {
        bind_interface: Some("ruscello-invalid-iface-000".to_owned()),
        ..updated.clone()
    };
    let err = match leecher.set_torrent_settings(invalid).await {
        Ok(()) => return Err("invalid interface should return error".into()),
        Err(err) => err,
    };
    #[cfg(windows)]
    assert!(matches!(
        err,
        EngineError::Torrent(TorrentError::InterfaceBindingUnsupported)
    ));
    #[cfg(not(windows))]
    assert!(matches!(
        err,
        EngineError::Torrent(TorrentError::InvalidInterface(_))
    ));
    assert_eq!(leecher.snapshot().await?.torrent, updated);

    leecher.shutdown().await?;
    seeder.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn magnet_category_rule_restart_fastresume_force_recheck_and_cancel_delete() -> TestResult {
    let seeder_dir = TempDir::new()?;
    let src_file = seeder_dir.path().join("ubuntu.iso");
    let payload = deterministic_bytes(99, 16_384 * 8);
    std::fs::write(&src_file, &payload)?;

    let seeder = EngineHandle::start(torrent_options(seeder_dir.path())).await?;
    let (_bytes, info_hash, seeder_addr) = seeder
        .create_and_serve_local_torrent(&src_file, Vec::new(), Some(16_384))
        .await?;

    let leecher_dir = TempDir::new()?;
    let leecher_opts = torrent_options(leecher_dir.path());
    let torrent_cat_dir = leecher_dir.path().join("torrents-out");
    std::fs::create_dir_all(&torrent_cat_dir)?;

    let magnet = format!("magnet:?xt=urn:btih:{info_hash}&dn=ubuntu.iso");

    let id = {
        let engine = EngineHandle::start(leecher_opts.clone()).await?;
        let mut settings = engine.snapshot().await?.categories;
        if let Some(policy) = settings
            .categories
            .iter_mut()
            .find(|p| p.category.id == DefaultCategory::Software.id())
        {
            policy.category.destination = Some(torrent_cat_dir.clone());
        }
        settings.rules.push(CategoryRule {
            matcher: CategoryMatch::Source(SourceKind::Torrent),
            category: DefaultCategory::Software.id(),
        });
        engine.set_categories(settings).await?;

        let mut events = engine.subscribe();
        let mut req = AddDownload::new(Source::Torrent(TorrentSource::Magnet(magnet.clone())));
        req.initial_peers = vec![seeder_addr];
        let id = engine.add(req).await?;

        wait_for_status(
            &engine,
            &mut events,
            id,
            Status::Seeding,
            Duration::from_secs(35),
        )
        .await?;

        let snap = engine.snapshot().await?;
        let dl = find_download(&snap.downloads, id)?;
        assert_eq!(dl.category, Some(DefaultCategory::Software.id()));
        assert_eq!(dl.file_type, Some(FileType::Torrents));
        assert_eq!(std::fs::read(torrent_cat_dir.join("ubuntu.iso"))?, payload);

        engine.pause(id).await?;
        wait_for_status(
            &engine,
            &mut events,
            id,
            Status::Paused,
            Duration::from_secs(10),
        )
        .await?;
        engine.shutdown().await?;
        id
    };

    // Reopen engine, corrupt downloaded file on disk, resume and force recheck, then cancel with delete.
    {
        let engine = EngineHandle::start(leecher_opts).await?;
        let mut events = engine.subscribe();
        let snap = engine.snapshot().await?;
        let dl = find_download(&snap.downloads, id)?;
        assert_eq!(dl.status(), Status::Paused);
        assert_eq!(dl.category, Some(DefaultCategory::Software.id()));

        let out_file = torrent_cat_dir.join("ubuntu.iso");
        let mut corrupted = std::fs::read(&out_file)?;
        for b in &mut corrupted[..8192] {
            *b ^= 0xFF;
        }
        std::fs::write(&out_file, &corrupted)?;

        #[cfg(windows)]
        let bitfield = {
            use std::os::windows::fs::OpenOptionsExt;

            // Keep the old handle in the delete/re-add window across scheduler ticks.
            const SHARE_READ_WRITE: u32 = 0x1 | 0x2;
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(SHARE_READ_WRITE)
                .open(
                    leecher_dir
                        .path()
                        .join("data/torrent")
                        .join(format!("{info_hash}.bitv")),
                )?
        };
        engine.force_recheck(id).await?;
        #[cfg(windows)]
        {
            assert!(matches!(
                engine.force_recheck(id).await,
                Err(EngineError::DownloadBusy)
            ));
            let pending = wait_for_status(
                &engine,
                &mut events,
                id,
                Status::Seeding,
                Duration::from_millis(900),
            )
            .await;
            let Err(error) = pending else {
                return Err("recheck finished before the bitfield closed".into());
            };
            assert!(error.is::<tokio::time::error::Elapsed>(), "{error}");
            let snapshot = engine.snapshot().await?;
            assert_eq!(
                find_download(&snapshot.downloads, id)?.status(),
                Status::Resolving
            );
            drop(bitfield);
        }
        wait_for_status(
            &engine,
            &mut events,
            id,
            Status::Seeding,
            Duration::from_secs(35),
        )
        .await?;
        assert_eq!(std::fs::read(&out_file)?, payload);

        engine.stop(id).await?;
        wait_for_status(
            &engine,
            &mut events,
            id,
            Status::Completed,
            Duration::from_secs(10),
        )
        .await?;
        let snap = engine.snapshot().await?;
        let stopped_dl = find_download(&snap.downloads, id)?;
        assert_eq!(stopped_dl.status(), Status::Completed);
        assert_eq!(std::fs::read(&out_file)?, payload);

        engine.resume(id).await?;
        wait_for_status(
            &engine,
            &mut events,
            id,
            Status::Seeding,
            Duration::from_secs(20),
        )
        .await?;

        engine.cancel(id, true).await?;
        timeout(Duration::from_secs(10), async {
            loop {
                match events.recv().await {
                    Ok(EngineEvent::Removed(removed)) if removed == id => {
                        return Ok::<_, Box<dyn Error>>(());
                    }
                    Ok(_) => {}
                    Err(err) => return Err(Box::new(err) as Box<dyn Error>),
                }
            }
        })
        .await??;

        assert!(engine.snapshot().await?.downloads.is_empty());
        assert!(
            !out_file.exists(),
            "cancel(delete_files=true) must remove files"
        );
        engine.shutdown().await?;
    }

    seeder.shutdown().await?;
    Ok(())
}
