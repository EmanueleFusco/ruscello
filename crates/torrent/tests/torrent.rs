use std::{
    error::Error,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use ruscello_core::{DownloadId, TorrentSource};
use ruscello_torrent::{
    AddTorrentRequest, TorrentError, TorrentFilePriority, TorrentInspection, TorrentManager,
    TorrentPhase, TorrentProgressSnapshot, TorrentSettings,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn Error>>;

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

async fn poll_until(
    manager: &TorrentManager,
    id: DownloadId,
    timeout: Duration,
    predicate: impl Fn(&TorrentProgressSnapshot) -> bool,
) -> Result<TorrentProgressSnapshot, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(snap) = manager.poll_progress(id)
            && predicate(&snap)
        {
            return Ok(snap);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for torrent {id:?} to satisfy predicate (last: {:?})",
                manager.poll_progress(id)
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

async fn serve_bytes_once(payload: Vec<u8>, path_hint: &str) -> Result<String, Box<dyn Error>> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let addr = listener.local_addr()?;
    let url = format!("http://{addr}/{path_hint}");
    tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/x-bittorrent\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            );
            let _ = stream.write_all(header.as_bytes()).await;
            let _ = stream.write_all(&payload).await;
            let _ = stream.shutdown().await;
        }
    });
    Ok(url)
}

fn find_relative_file(
    inspection: &TorrentInspection,
    suffix: &str,
) -> Result<usize, Box<dyn Error>> {
    inspection
        .files
        .iter()
        .find(|f| {
            f.path
                .to_string_lossy()
                .replace('\\', "/")
                .ends_with(suffix)
        })
        .map(|f| f.index)
        .ok_or_else(|| format!("missing inspected file ending in {suffix}").into())
}

fn resolved_file_path(root: &Path, torrent_name: &str, rel: &str) -> PathBuf {
    root.join(torrent_name).join(rel)
}

#[tokio::test]
async fn inspect_and_selective_file_download_with_details() -> TestResult {
    let seeder_dir = TempDir::new()?;
    let src_dir = seeder_dir.path().join("bundle");
    std::fs::create_dir_all(src_dir.join("nested"))?;

    let alpha = deterministic_bytes(1, 16_384 * 6);
    let beta = deterministic_bytes(2, 16_384 * 4);
    let skip = deterministic_bytes(3, 16_384 * 4);
    std::fs::write(src_dir.join("alpha.bin"), &alpha)?;
    std::fs::write(src_dir.join("nested").join("beta.bin"), &beta)?;
    std::fs::write(src_dir.join("skip_me.bin"), &skip)?;

    let seeder = TorrentManager::new(
        seeder_dir.path().join("data"),
        seeder_dir.path().join("dest"),
        TorrentSettings::offline_ephemeral(),
    )
    .await?;

    let (torrent_bytes, info_hash, seeder_addr) = seeder
        .create_and_serve_local(&src_dir, Vec::new(), Some(16_384))
        .await?;

    let torrent_file = seeder_dir.path().join("bundle.torrent");
    std::fs::write(&torrent_file, &torrent_bytes)?;

    let leecher_dir = TempDir::new()?;
    let leecher_dest = leecher_dir.path().join("downloads");
    let leecher = TorrentManager::new(
        leecher_dir.path().join("data"),
        leecher_dest.clone(),
        TorrentSettings::offline_ephemeral(),
    )
    .await?;

    let cancel = CancellationToken::new();
    let inspected = leecher
        .inspect(
            &TorrentSource::File(torrent_file.clone()),
            Vec::new(),
            &cancel,
        )
        .await?;

    assert_eq!(inspected.info_hash, info_hash);
    assert_eq!(inspected.files.len(), 3);
    assert_eq!(
        inspected.total_bytes,
        (alpha.len() + beta.len() + skip.len()) as u64
    );
    assert_eq!(inspected.piece_length, 16_384);
    assert!(inspected.piece_count >= 14);

    let alpha_idx = find_relative_file(&inspected, "alpha.bin")?;
    let beta_idx = find_relative_file(&inspected, "nested/beta.bin")?;
    let skip_idx = find_relative_file(&inspected, "skip_me.bin")?;

    let id = DownloadId(42);
    let started = leecher
        .start_or_resume(
            AddTorrentRequest {
                id,
                source: TorrentSource::File(torrent_file),
                destination: leecher_dest.clone(),
                resolved_output_folder: None,
                cached_torrent_bytes: inspected.torrent_bytes.clone(),
                only_files: Some(vec![alpha_idx, beta_idx]),
                paused: false,
                force_recheck: false,
                prior_uploaded_bytes: 0,
                prior_seed_elapsed_secs: 0,
                seed_ratio_millis: None,
                seed_time_limit_minutes: None,
                download_limit_bps: None,
                initial_peers: vec![seeder_addr],
            },
            &cancel,
        )
        .await?;

    let selected_total = (alpha.len() + beta.len()) as u64;
    assert_eq!(started.info_hash, info_hash);

    let snap = poll_until(&leecher, id, Duration::from_secs(20), |snap| {
        matches!(snap.phase, TorrentPhase::Seeding) && snap.downloaded_bytes >= selected_total
    })
    .await?;
    assert_eq!(snap.total_bytes, Some(selected_total));

    let alpha_out = resolved_file_path(&leecher_dest, &inspected.name, "alpha.bin");
    let beta_out = resolved_file_path(&leecher_dest, &inspected.name, "nested/beta.bin");
    let skip_out = resolved_file_path(&leecher_dest, &inspected.name, "skip_me.bin");

    assert_eq!(std::fs::read(&alpha_out)?, alpha);
    assert_eq!(std::fs::read(&beta_out)?, beta);
    if skip_out.exists() {
        assert_ne!(
            std::fs::read(&skip_out)?,
            skip,
            "skipped file should not receive payload bytes"
        );
    }

    let details = leecher.details(id).await?;
    assert_eq!(details.info_hash, info_hash);
    assert_eq!(details.files.len(), 3);
    let skip_entry = details
        .files
        .iter()
        .find(|f| f.index == skip_idx)
        .ok_or("missing skip file entry")?;
    assert_eq!(skip_entry.priority, TorrentFilePriority::Skip);
    assert_eq!(skip_entry.downloaded_bytes, 0);
    assert_eq!(
        details
            .files
            .iter()
            .find(|f| f.index == alpha_idx)
            .ok_or("missing alpha file entry")?
            .priority,
        TorrentFilePriority::Download
    );
    let piece_map = details.piece_map.ok_or("missing piece map")?;
    assert_eq!(piece_map.piece_count, inspected.piece_count);
    assert!(piece_map.have_count > 0);
    assert!(
        !details.peers.is_empty(),
        "expected at least the seeder peer entry"
    );

    // Enable the previously skipped file dynamically and ensure it downloads completely.
    leecher
        .set_only_files(id, vec![alpha_idx, beta_idx, skip_idx])
        .await?;

    let full_total = selected_total + skip.len() as u64;
    poll_until(&leecher, id, Duration::from_secs(20), |snap| {
        matches!(snap.phase, TorrentPhase::Seeding) && snap.downloaded_bytes >= full_total
    })
    .await?;
    assert_eq!(std::fs::read(&skip_out)?, skip);

    leecher.shutdown().await;
    seeder.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn magnet_http_torrent_url_and_restart_fastresume() -> TestResult {
    let seeder_dir = TempDir::new()?;
    let src_file = seeder_dir.path().join("archive.bin");
    let payload = deterministic_bytes(77, 16_384 * 12);
    std::fs::write(&src_file, &payload)?;

    let seeder = TorrentManager::new(
        seeder_dir.path().join("data"),
        seeder_dir.path().join("dest"),
        TorrentSettings::offline_ephemeral(),
    )
    .await?;

    let (torrent_bytes, info_hash, seeder_addr) = seeder
        .create_and_serve_local(&src_file, Vec::new(), Some(16_384))
        .await?;

    let cancel = CancellationToken::new();

    // 1. Inspect `.torrent` served over HTTP URL.
    let torrent_url = serve_bytes_once(torrent_bytes.clone(), "archive.torrent").await?;
    let url_inspect = leecher_inspect_url(seeder_dir.path(), &torrent_url, &cancel).await?;
    assert_eq!(url_inspect.info_hash, info_hash);
    assert_eq!(url_inspect.total_bytes, payload.len() as u64);

    // 2. Inspect and download via magnet URI + loopback initial_peers, then pause/restart/reconcile.
    let leecher_dir = TempDir::new()?;
    let leecher_data = leecher_dir.path().join("data");
    let leecher_dest = leecher_dir.path().join("dest");
    let magnet = format!("magnet:?xt=urn:btih:{info_hash}&dn=archive.bin");

    let id = DownloadId(99);
    {
        let leecher = TorrentManager::new(
            leecher_data.clone(),
            leecher_dest.clone(),
            TorrentSettings::offline_ephemeral(),
        )
        .await?;

        let magnet_inspection = leecher
            .inspect(
                &TorrentSource::Magnet(magnet.clone()),
                vec![seeder_addr],
                &cancel,
            )
            .await?;
        assert_eq!(magnet_inspection.info_hash, info_hash);
        assert_eq!(magnet_inspection.total_bytes, payload.len() as u64);

        leecher
            .start_or_resume(
                AddTorrentRequest {
                    id,
                    source: TorrentSource::Magnet(magnet.clone()),
                    destination: leecher_dest.clone(),
                    resolved_output_folder: None,
                    cached_torrent_bytes: None,
                    only_files: None,
                    paused: false,
                    force_recheck: false,
                    prior_uploaded_bytes: 0,
                    prior_seed_elapsed_secs: 0,
                    seed_ratio_millis: None,
                    seed_time_limit_minutes: None,
                    download_limit_bps: None,
                    initial_peers: vec![seeder_addr],
                },
                &cancel,
            )
            .await?;

        poll_until(&leecher, id, Duration::from_secs(20), |snap| {
            matches!(snap.phase, TorrentPhase::Seeding)
                && snap.downloaded_bytes == payload.len() as u64
        })
        .await?;

        leecher.pause(id).await?;
        leecher.shutdown().await;
    }

    // Reopen session pointing to same persistence directory and verify reconcile + fastresume.
    {
        let reopened = TorrentManager::new(
            leecher_data,
            leecher_dest.clone(),
            TorrentSettings::offline_ephemeral(),
        )
        .await?;

        reopened.reconcile(&[(id, Some(info_hash.clone()))]).await;

        let paused_snap = reopened.poll_progress(id)?;
        assert_eq!(paused_snap.phase, TorrentPhase::Paused);

        reopened
            .start_or_resume(
                AddTorrentRequest {
                    id,
                    source: TorrentSource::Magnet(magnet),
                    destination: leecher_dest.clone(),
                    resolved_output_folder: None,
                    cached_torrent_bytes: Some(torrent_bytes),
                    only_files: None,
                    paused: false,
                    force_recheck: false,
                    prior_uploaded_bytes: 1_024,
                    prior_seed_elapsed_secs: 15,
                    seed_ratio_millis: Some(1500),
                    seed_time_limit_minutes: None,
                    download_limit_bps: None,
                    initial_peers: Vec::new(),
                },
                &cancel,
            )
            .await?;

        let resumed_snap = poll_until(&reopened, id, Duration::from_secs(10), |snap| {
            matches!(snap.phase, TorrentPhase::Seeding)
                && snap.downloaded_bytes == payload.len() as u64
        })
        .await?;
        assert!(resumed_snap.uploaded_bytes >= 1_024);
        assert!(resumed_snap.seed_elapsed_secs >= 15);
        assert_eq!(std::fs::read(leecher_dest.join("archive.bin"))?, payload);
        reopened.shutdown().await;
    }

    seeder.shutdown().await;
    Ok(())
}

async fn leecher_inspect_url(
    base_dir: &Path,
    url: &str,
    cancel: &CancellationToken,
) -> Result<TorrentInspection, Box<dyn Error>> {
    let temp = TorrentManager::new(
        base_dir.join("url-inspect-data"),
        base_dir.join("url-inspect-dest"),
        TorrentSettings::offline_ephemeral(),
    )
    .await?;
    let inspected = temp
        .inspect(&TorrentSource::Url(url.to_owned()), Vec::new(), cancel)
        .await?;
    temp.shutdown().await;
    Ok(inspected)
}

#[tokio::test]
async fn force_recheck_detects_corruption_and_repairs() -> TestResult {
    let seeder_dir = TempDir::new()?;
    let src_file = seeder_dir.path().join("image.iso");
    let payload = deterministic_bytes(555, 16_384 * 10);
    std::fs::write(&src_file, &payload)?;

    let seeder = TorrentManager::new(
        seeder_dir.path().join("data"),
        seeder_dir.path().join("dest"),
        TorrentSettings::offline_ephemeral(),
    )
    .await?;
    let (torrent_bytes, _hash, seeder_addr) = seeder
        .create_and_serve_local(&src_file, Vec::new(), Some(16_384))
        .await?;

    let leecher_dir = TempDir::new()?;
    let leecher_dest = leecher_dir.path().join("dest");
    let leecher = TorrentManager::new(
        leecher_dir.path().join("data"),
        leecher_dest.clone(),
        TorrentSettings::offline_ephemeral(),
    )
    .await?;

    let torrent_path = leecher_dir.path().join("image.torrent");
    std::fs::write(&torrent_path, &torrent_bytes)?;

    let cancel = CancellationToken::new();
    let id = DownloadId(7);
    let base_req = AddTorrentRequest {
        id,
        source: TorrentSource::File(torrent_path),
        destination: leecher_dest.clone(),
        resolved_output_folder: None,
        cached_torrent_bytes: Some(torrent_bytes),
        only_files: None,
        paused: false,
        force_recheck: false,
        prior_uploaded_bytes: 0,
        prior_seed_elapsed_secs: 0,
        seed_ratio_millis: None,
        seed_time_limit_minutes: None,
        download_limit_bps: None,
        initial_peers: vec![seeder_addr],
    };

    let started = leecher.start_or_resume(base_req.clone(), &cancel).await?;

    poll_until(&leecher, id, Duration::from_secs(20), |snap| {
        matches!(snap.phase, TorrentPhase::Seeding) && snap.downloaded_bytes == payload.len() as u64
    })
    .await?;

    leecher.pause(id).await?;

    // Corrupt piece 0 on disk while paused.
    let mut disk_bytes = std::fs::read(&started.completed_path)?;
    for byte in &mut disk_bytes[..8192] {
        *byte ^= 0xFF;
    }
    std::fs::write(&started.completed_path, &disk_bytes)?;
    assert_ne!(std::fs::read(&started.completed_path)?, payload);

    // Trigger force_recheck and wait for automatic repair back to Seeding with clean bytes.
    leecher.force_recheck(base_req, &cancel).await?;

    poll_until(&leecher, id, Duration::from_secs(20), |snap| {
        matches!(snap.phase, TorrentPhase::Seeding)
            && snap.downloaded_bytes == payload.len() as u64
            && std::fs::read(&started.completed_path).is_ok_and(|cur| cur == payload)
    })
    .await?;

    leecher.shutdown().await;
    seeder.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn seed_limits_blocklist_and_invalid_interface() -> TestResult {
    let seeder_dir = TempDir::new()?;
    let src_file = seeder_dir.path().join("seed.dat");
    let payload = deterministic_bytes(9001, 16_384 * 6);
    std::fs::write(&src_file, &payload)?;

    let seeder = TorrentManager::new(
        seeder_dir.path().join("data"),
        seeder_dir.path().join("dest"),
        TorrentSettings::offline_ephemeral(),
    )
    .await?;
    let (torrent_bytes, _hash, seeder_addr) = seeder
        .create_and_serve_local(&src_file, Vec::new(), Some(16_384))
        .await?;

    let cancel = CancellationToken::new();

    // 1. Seed limits (`seed_ratio_millis: Some(0)` and `seed_time_limit_minutes: Some(0)`).
    let leecher_dir = TempDir::new()?;
    let leecher_dest = leecher_dir.path().join("dest");
    let leecher = TorrentManager::new(
        leecher_dir.path().join("data"),
        leecher_dest.clone(),
        TorrentSettings::offline_ephemeral(),
    )
    .await?;

    let torrent_path = leecher_dir.path().join("seed.torrent");
    std::fs::write(&torrent_path, &torrent_bytes)?;

    let id = DownloadId(101);
    leecher
        .start_or_resume(
            AddTorrentRequest {
                id,
                source: TorrentSource::File(torrent_path.clone()),
                destination: leecher_dest,
                resolved_output_folder: None,
                cached_torrent_bytes: Some(torrent_bytes.clone()),
                only_files: None,
                paused: false,
                force_recheck: false,
                prior_uploaded_bytes: 0,
                prior_seed_elapsed_secs: 0,
                seed_ratio_millis: Some(0),
                seed_time_limit_minutes: None,
                download_limit_bps: None,
                initial_peers: vec![seeder_addr],
            },
            &cancel,
        )
        .await?;

    let finished = poll_until(&leecher, id, Duration::from_secs(20), |snap| {
        snap.seed_goal_reached
    })
    .await?;
    assert_eq!(finished.phase, TorrentPhase::Seeding);
    assert!(finished.seed_goal_reached);

    leecher.set_torrent_seed_limits(id, None, Some(0));
    let time_limited = leecher.poll_progress(id)?;
    assert!(time_limited.seed_goal_reached);

    leecher.set_torrent_seed_limits(id, Some(5000), Some(60));
    let relaxed = leecher.poll_progress(id)?;
    assert!(!relaxed.seed_goal_reached);
    leecher.shutdown().await;

    // 2. IP blocklist blocking loopback seeder address.
    let blocked_dir = TempDir::new()?;
    let blocklist_file = blocked_dir.path().join("blocklist.p2p");
    std::fs::write(
        &blocklist_file,
        "LoopbackV4:127.0.0.1-127.0.0.2\nLoopbackV6:::1-::2\n",
    )?;

    let blocked_settings = TorrentSettings {
        blocklist_source: Some(blocklist_file.to_string_lossy().into_owned()),
        ..TorrentSettings::offline_ephemeral()
    };
    let blocked_leecher = TorrentManager::new(
        blocked_dir.path().join("data"),
        blocked_dir.path().join("dest"),
        blocked_settings,
    )
    .await?;

    let blocked_id = DownloadId(102);
    blocked_leecher
        .start_or_resume(
            AddTorrentRequest {
                id: blocked_id,
                source: TorrentSource::File(torrent_path),
                destination: blocked_dir.path().join("dest"),
                resolved_output_folder: None,
                cached_torrent_bytes: Some(torrent_bytes),
                only_files: None,
                paused: false,
                force_recheck: false,
                prior_uploaded_bytes: 0,
                prior_seed_elapsed_secs: 0,
                seed_ratio_millis: None,
                seed_time_limit_minutes: None,
                download_limit_bps: None,
                initial_peers: vec![seeder_addr],
            },
            &cancel,
        )
        .await?;

    tokio::time::sleep(Duration::from_millis(800)).await;
    let snap = blocked_leecher.poll_progress(blocked_id)?;
    assert_eq!(
        snap.downloaded_bytes, 0,
        "loopback seeder should remain blocked by P2P IP blocklist"
    );
    blocked_leecher.shutdown().await;

    // 3. Invalid bind interface returns an error without panicking.
    let bad_iface_dir = TempDir::new()?;
    let bad_iface = TorrentSettings {
        bind_interface: Some("ruscello-nonexistent-nic-999".to_owned()),
        ..TorrentSettings::offline_ephemeral()
    };
    let result = TorrentManager::new(
        bad_iface_dir.path().join("data"),
        bad_iface_dir.path().join("dest"),
        bad_iface,
    )
    .await;
    assert!(
        matches!(
            result,
            Err(TorrentError::InvalidInterface(_) | TorrentError::Session(_))
        ),
        "expected invalid interface error"
    );

    seeder.shutdown().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires live internet and DHT connectivity"]
async fn live_dht_magnet_inspection() -> TestResult {
    let dir = TempDir::new()?;
    let settings = TorrentSettings {
        listen_port: 0,
        ..Default::default()
    };
    let manager =
        TorrentManager::new(dir.path().join("data"), dir.path().join("dest"), settings).await?;

    let telemetry = manager.session_telemetry().await;
    assert!(telemetry.listen_port.is_some());
    assert!(telemetry.dht_nodes.is_some());
    let _source = TorrentSource::Magnet(
        "magnet:?xt=urn:btih:0000000000000000000000000000000000000000&dn=example".into(),
    );
    let _: SocketAddr = (Ipv4Addr::LOCALHOST, 0).into();
    manager.shutdown().await;
    Ok(())
}
