use std::{
    error::Error,
    fmt,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use ruscello_core::{
    DefaultCategory, Download, DownloadId, FileType, Source, SourceKind, Status, TorrentSource,
};
use ruscello_engine::{
    AddDownload, CategoryMatch, CategoryRule, EngineError, EngineEvent, EngineHandle,
    EngineOptions, TorrentError, TorrentFilePriority, TorrentSettings,
};
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
    assert!(matches!(
        err,
        EngineError::Torrent(TorrentError::InvalidInterface(_) | TorrentError::Session(_))
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

        engine.force_recheck(id).await?;
        wait_for_status(
            &engine,
            &mut events,
            id,
            Status::Seeding,
            Duration::from_secs(35),
        )
        .await?;
        assert_eq!(std::fs::read(&out_file)?, payload);

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
