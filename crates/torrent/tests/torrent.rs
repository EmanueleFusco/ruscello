use std::{
    error::Error,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    num::{NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use ruscello_core::{DownloadId, TorrentSource};
use ruscello_torrent::{
    AddTorrentRequest, TorrentError, TorrentFilePriority, TorrentInspection, TorrentManager,
    TorrentPeerTransport, TorrentPhase, TorrentProgressSnapshot, TorrentSettings,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::mpsc,
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
            let last_status = manager.poll_progress(id).map(|snapshot| {
                format!(
                    "{:?}, {} bytes downloaded, {} peers",
                    snapshot.phase, snapshot.downloaded_bytes, snapshot.peers_connected
                )
            });
            return Err(
                format!("timed out waiting for torrent {id:?} (last: {last_status:?})").into(),
            );
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

fn local_request(
    id: DownloadId,
    path: PathBuf,
    bytes: Vec<u8>,
    destination: &Path,
    peer: SocketAddr,
    download_limit_bps: Option<NonZeroU64>,
) -> AddTorrentRequest {
    AddTorrentRequest {
        id,
        source: TorrentSource::File(path),
        destination: destination.to_path_buf(),
        resolved_output_folder: None,
        cached_torrent_bytes: Some(bytes),
        only_files: None,
        paused: false,
        force_recheck: false,
        prior_uploaded_bytes: 0,
        prior_seed_elapsed_secs: 0,
        seed_ratio_millis: None,
        seed_time_limit_minutes: None,
        download_limit_bps,
        initial_peers: vec![peer],
    }
}

#[tokio::test]
async fn metadata_lookup_shares_the_budget_and_cancellation_releases_it() -> TestResult {
    use std::sync::Arc;

    let root = TempDir::new()?;
    let seed = TorrentManager::new(
        root.path().join("metadata-seed"),
        root.path().join("metadata-source"),
        TorrentSettings::offline_ephemeral(),
    )
    .await?;
    let source = root.path().join("metadata.bin");
    let contents = deterministic_bytes(86, 128 * 1024);
    std::fs::write(&source, &contents)?;
    let (_, hash, peer) = seed.create_and_serve_local(&source, vec![], None).await?;
    let dest = root.path().join("metadata-download");
    let receiver = Arc::new(
        TorrentManager::new(
            root.path().join("metadata-receiver"),
            dest.clone(),
            TorrentSettings {
                max_global_peers: NonZeroUsize::MIN,
                ..TorrentSettings::offline_ephemeral()
            },
        )
        .await?,
    );
    let silent_peer = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let silent_address = silent_peer.local_addr()?;
    let (accepted, mut connections) = mpsc::channel(1);
    let silent_task = tokio::spawn(async move {
        let (mut socket, _) = silent_peer.accept().await?;
        let mut handshake = [0; 68];
        socket.read_exact(&mut handshake).await?;
        let _ = accepted.send(()).await;
        let _ = socket.read(&mut handshake).await;
        Ok::<_, std::io::Error>(())
    });
    let cancellation = CancellationToken::new();
    let first_cancel = cancellation.clone();
    let first_receiver = receiver.clone();
    let first = tokio::spawn(async move {
        first_receiver
            .inspect(
                &TorrentSource::Magnet(
                    "magnet:?xt=urn:btih:1111111111111111111111111111111111111111".into(),
                ),
                vec![silent_address],
                &first_cancel,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), connections.recv())
        .await?
        .ok_or("silent metadata peer did not accept the handshake")?;

    let second_receiver = receiver.clone();
    let mut second = tokio::spawn(async move {
        second_receiver
            .inspect(
                &TorrentSource::Magnet(format!("magnet:?xt=urn:btih:{hash}")),
                vec![peer],
                &CancellationToken::new(),
            )
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(500), &mut second)
            .await
            .is_err(),
        "the second lookup bypassed the session budget"
    );
    cancellation.cancel();
    assert!(matches!(first.await?, Err(TorrentError::Cancelled)));
    let inspection = tokio::time::timeout(Duration::from_secs(10), second).await???;
    let bytes = inspection
        .torrent_bytes
        .ok_or("missing resolved metadata")?;
    let started = receiver
        .start_or_resume(
            local_request(DownloadId(86), source, bytes, &dest, peer, None),
            &CancellationToken::new(),
        )
        .await?;
    poll_until(&receiver, DownloadId(86), Duration::from_secs(10), |snap| {
        matches!(snap.phase, TorrentPhase::Seeding)
    })
    .await?;
    assert_eq!(std::fs::read(started.completed_path)?, contents);
    tokio::time::timeout(Duration::from_secs(5), silent_task).await???;
    receiver.shutdown().await;
    seed.shutdown().await;
    Ok(())
}

#[test]
fn older_torrent_settings_keep_limits_and_receive_new_defaults() -> TestResult {
    let older: TorrentSettings = serde_json::from_str(
        r#"{"listen_port":0,"max_peers_per_torrent":24,"download_limit_bps":262144}"#,
    )?;
    assert!(!older.enable_utp);
    assert_eq!(older.max_peers_per_torrent.map(NonZeroUsize::get), Some(24));
    assert_eq!(older.max_global_peers.get(), 500);
    assert_eq!(older.download_limit_bps.map(NonZeroU64::get), Some(262144));
    let changed = TorrentSettings {
        enable_utp: true,
        max_global_peers: NonZeroUsize::MIN.saturating_add(11),
        ..older.clone()
    };
    assert!(older.needs_session_restart(&changed));
    let restored: TorrentSettings = serde_json::from_slice(&serde_json::to_vec(&changed)?)?;
    assert_eq!(changed, restored);
    Ok(())
}

#[tokio::test]
async fn outgoing_peer_budget_is_shared_and_released_on_pause() -> TestResult {
    exercise_shared_peer_budget(false).await
}

#[tokio::test]
async fn incoming_peer_budget_is_shared_and_released_on_pause() -> TestResult {
    exercise_shared_peer_budget(true).await
}

async fn exercise_shared_peer_budget(limit_seed: bool) -> TestResult {
    let root = TempDir::new()?;
    let settings = TorrentSettings::offline_ephemeral();
    let limited = TorrentSettings {
        max_global_peers: NonZeroUsize::MIN,
        ..settings.clone()
    };
    let (seed_settings, receiver_settings) = if limit_seed {
        (limited, settings)
    } else {
        (settings, limited)
    };
    let seed = TorrentManager::new(
        root.path().join("seed-state"),
        root.path().join("seed"),
        seed_settings,
    )
    .await?;
    let dest = root.path().join("downloads");
    let receiver = TorrentManager::new(
        root.path().join("receiver-state"),
        dest.clone(),
        receiver_settings,
    )
    .await?;
    let cancel = CancellationToken::new();
    let first = DownloadId(1);
    let second = DownloadId(2);
    let first_path = root.path().join("first.bin");
    let second_path = root.path().join("second.bin");
    let second_contents = deterministic_bytes(12, 128 * 1024);
    std::fs::write(&first_path, deterministic_bytes(11, 1024 * 1024))?;
    std::fs::write(&second_path, &second_contents)?;
    let (first_torrent, _, peer) = seed
        .create_and_serve_local(&first_path, vec![], Some(16_384))
        .await?;
    let (second_torrent, _, _) = seed
        .create_and_serve_local(&second_path, vec![], None)
        .await?;
    receiver
        .start_or_resume(
            local_request(
                first,
                first_path,
                first_torrent,
                &dest,
                peer,
                NonZeroU64::new(32 * 1024),
            ),
            &cancel,
        )
        .await?;
    poll_until(&receiver, first, Duration::from_secs(12), |snap| {
        snap.downloaded_bytes > 0 && snap.peers_connected == 1
    })
    .await?;

    let second_request = local_request(second, second_path, second_torrent, &dest, peer, None);
    let mut started = receiver
        .start_or_resume(second_request.clone(), &cancel)
        .await?;
    poll_until(&receiver, second, Duration::from_secs(12), |snap| {
        matches!(snap.phase, TorrentPhase::Downloading)
    })
    .await?;
    assert!(
        poll_until(&receiver, second, Duration::from_millis(800), |snap| {
            snap.downloaded_bytes != 0 || (!limit_seed && snap.peers_connected != 0)
        })
        .await
        .is_err(),
        "the second torrent got a peer while the session budget was exhausted"
    );
    assert_eq!(receiver.poll_progress(first)?.peers_connected, 1);
    assert_eq!(receiver.poll_progress(second)?.downloaded_bytes, 0);
    if !limit_seed {
        assert_eq!(receiver.poll_progress(second)?.peers_connected, 0);
    }
    receiver.pause(first).await?;
    if limit_seed {
        // Reopen the connection after the seed rejected the first handshake.
        // A real swarm can also retry the peer after a subsequent tracker announce.
        receiver.remove(second, false).await?;
        started = receiver.start_or_resume(second_request, &cancel).await?;
    }
    poll_until(&receiver, second, Duration::from_secs(12), |snap| {
        matches!(snap.phase, TorrentPhase::Seeding)
    })
    .await?;
    assert_eq!(std::fs::read(started.completed_path)?, second_contents);
    receiver.shutdown().await;
    seed.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn tcp_and_utp_mode_resumes_when_tcp_accepts_without_a_peer_handshake() -> TestResult {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use librqbit::{CreateTorrentOptions, ListenerMode, ListenerOptions, Session, SessionOptions};

    let root = TempDir::new()?;
    let data = deterministic_bytes(13, 4 * 1024 * 1024);
    let source = root.path().join("utp-only.bin");
    std::fs::write(&source, &data)?;
    let seed = Session::new_with_opts(
        root.path().join("seed"),
        SessionOptions {
            dht: None,
            listen: Some(ListenerOptions {
                listen_addr: (Ipv4Addr::LOCALHOST, 0).into(),
                mode: ListenerMode::UtpOnly,
                ..Default::default()
            }),
            disable_local_service_discovery: true,
            ..Default::default()
        },
    )
    .await?;
    let (torrent, handle) = seed
        .create_and_serve_torrent(
            &source,
            CreateTorrentOptions {
                name: None,
                trackers: vec![],
                piece_length: Some(64 * 1024),
            },
        )
        .await?;
    handle.wait_until_initialized().await?;
    let seed_address = seed
        .listen_addr()
        .ok_or("µTP seeder has no listening address")?;
    let relay = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let address = relay.local_addr()?;
    let (report_syn, mut syns) = mpsc::unbounded_channel();
    let acknowledgments = Arc::new(AtomicUsize::new(0));
    let observed_acks = acknowledgments.clone();
    let relay_task = tokio::spawn(async move {
        let mut data = [0u8; 2048];
        let mut receiver = None;
        let mut dropped_syns = 0;
        while let Ok((length, sender)) = relay.recv_from(&mut data).await {
            let target = if sender == seed_address {
                let Some(receiver) = receiver else {
                    continue;
                };
                receiver
            } else {
                receiver = Some(sender);
                // BEP 29 defines ST_STATE packets as acknowledgments without payload.
                if length >= 20 && data[0] == 0x21 {
                    observed_acks.fetch_add(1, Ordering::Relaxed);
                }
                if length >= 20 && data[0] == 0x41 && dropped_syns < 3 {
                    let fields = ([data[2], data[3]], [data[16], data[17]]);
                    let _ = report_syn.send(fields);
                    dropped_syns += 1;
                    if dropped_syns <= 2 {
                        continue;
                    }
                }
                seed_address
            };
            let _ = relay.send_to(&data[..length], target).await;
        }
    });
    let tcp_decoy = TcpListener::bind(address).await?;
    let (received_handshake, mut handshakes) = mpsc::unbounded_channel();
    let tcp_task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = tcp_decoy.accept().await {
            let received_handshake = received_handshake.clone();
            tokio::spawn(async move {
                let mut handshake = [0; 68];
                if socket.read_exact(&mut handshake).await.is_ok() {
                    let _ = received_handshake.send(());
                    let _ = socket.read(&mut handshake).await;
                }
            });
        }
    });
    let dest = root.path().join("downloads");
    let tcp_port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await?
        .local_addr()?
        .port();
    let bootstrap = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let receiver = TorrentManager::new(
        root.path().join("receiver"),
        dest.clone(),
        TorrentSettings {
            listen_port: tcp_port,
            enable_utp: true,
            enable_dht: true,
            max_global_peers: NonZeroUsize::MIN,
            dht_bootstrap_addrs: Some(vec![bootstrap.local_addr()?.to_string()]),
            ..TorrentSettings::offline_ephemeral()
        },
    )
    .await?;
    assert!(receiver.session_telemetry().await.dht_nodes.is_some());
    let id = DownloadId(3);
    let request = local_request(
        id,
        source,
        torrent.as_bytes()?.to_vec(),
        &dest,
        address,
        NonZeroU64::new(512 * 1024),
    );
    let started = receiver
        .start_or_resume(request.clone(), &CancellationToken::new())
        .await?;
    tokio::time::timeout(Duration::from_secs(5), handshakes.recv())
        .await?
        .ok_or("TCP connection did not receive the BitTorrent handshake")?;
    let syn_ids = tokio::time::timeout(Duration::from_secs(7), async {
        let first = syns.recv().await?;
        let second = syns.recv().await?;
        let third = syns.recv().await?;
        Some([first, second, third])
    })
    .await?
    .ok_or("µTP did not retransmit both lost SYNs")?;
    assert_eq!(syn_ids, [syn_ids[0]; 3]);
    for threshold in [256 * 1024, 1024 * 1024] {
        poll_until(&receiver, id, Duration::from_secs(15), |snap| {
            snap.downloaded_bytes >= threshold && matches!(snap.phase, TorrentPhase::Downloading)
        })
        .await?;
        receiver.pause(id).await?;
        let paused = receiver.poll_progress(id)?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            receiver.poll_progress(id)?.downloaded_bytes,
            paused.downloaded_bytes
        );
        receiver
            .start_or_resume(request.clone(), &CancellationToken::new())
            .await?;
    }
    let completed = poll_until(&receiver, id, Duration::from_secs(20), |snap| {
        matches!(snap.phase, TorrentPhase::Seeding)
    })
    .await?;
    assert_eq!(std::fs::read(started.completed_path)?, data);
    assert!(acknowledgments.load(Ordering::Relaxed) > 0);
    assert_eq!(completed.uploaded_bytes, 0);
    tcp_task.abort();
    relay_task.abort();
    receiver.shutdown().await;
    seed.stop().await;
    Ok(())
}

#[tokio::test]
async fn restored_partial_contacts_tracker_and_resumes() -> TestResult {
    let root = TempDir::new()?;
    let seed = TorrentManager::new(
        root.path().join("seed-session"),
        root.path().join("seed-destination"),
        TorrentSettings::offline_ephemeral(),
    )
    .await?;
    let source = root.path().join("fixture.bin");
    let payload = deterministic_bytes(84, 2 * 1024 * 1024);
    std::fs::write(&source, &payload)?;
    let (_, _, peer) = seed
        .create_and_serve_local(&source, vec![], Some(64 * 1024))
        .await?;
    let tracker = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let url = format!("http://{}/announce", tracker.local_addr()?);
    let (announced, mut announcements) = mpsc::unbounded_channel();
    let tracker_task = tokio::spawn(async move {
        let [high, low] = peer.port().to_be_bytes();
        let mut body = b"d8:intervali60e5:peers6:".to_vec();
        body.extend([127, 0, 0, 1, high, low, b'e']);
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        while let Ok((mut socket, _)) = tracker.accept().await {
            let mut received = [0; 1024];
            if socket
                .read(&mut received)
                .await
                .is_ok_and(|count| count > 0)
            {
                let _ = announced.send(());
                let _ = socket.write_all(headers.as_bytes()).await;
                let _ = socket.write_all(&body).await;
            }
        }
    });
    let bytes = TorrentManager::build_torrent_bytes(&source, vec![url], Some(64 * 1024)).await?;
    let settings = TorrentSettings {
        download_limit_bps: NonZeroU64::new(512 * 1024),
        ..TorrentSettings::offline_ephemeral()
    };
    let session = root.path().join("session");
    let destination = root.path().join("downloads");
    let manager =
        TorrentManager::new(session.clone(), destination.clone(), settings.clone()).await?;
    let id = DownloadId(84);
    let cancellation = CancellationToken::new();
    let mut request = local_request(id, source, bytes, &destination, peer, None);
    request.initial_peers.clear();
    let started = manager
        .start_or_resume(request.clone(), &cancellation)
        .await?;
    let partial = poll_until(&manager, id, Duration::from_secs(10), |snapshot| {
        snapshot.downloaded_bytes >= 256 * 1024
            && matches!(snapshot.phase, TorrentPhase::Downloading)
    })
    .await?;
    manager.shutdown().await;
    drop(manager);
    while announcements.try_recv().is_ok() {}

    let restored = TorrentManager::new(session, destination, settings).await?;
    restored.reconcile(&[(id, Some(started.info_hash))]).await;
    restored.start_or_resume(request, &cancellation).await?;
    tokio::time::timeout(Duration::from_secs(4), announcements.recv())
        .await?
        .ok_or("restored session did not announce to the tracker")?;
    let completed = poll_until(&restored, id, Duration::from_secs(10), |snapshot| {
        matches!(snapshot.phase, TorrentPhase::Seeding)
    })
    .await?;
    assert!(partial.downloaded_bytes < completed.downloaded_bytes);
    assert_eq!(std::fs::read(started.completed_path)?, payload);
    restored.shutdown().await;
    tracker_task.abort();
    seed.shutdown().await;
    Ok(())
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

    let mut files = [
        (
            find_relative_file(&inspected, "alpha.bin")?,
            "alpha.bin",
            alpha,
        ),
        (
            find_relative_file(&inspected, "nested/beta.bin")?,
            "nested/beta.bin",
            beta,
        ),
        (
            find_relative_file(&inspected, "skip_me.bin")?,
            "skip_me.bin",
            skip,
        ),
    ];
    // Skip the first file in piece order even when the filesystem enumerates entries differently.
    files.sort_by_key(|(index, _, _)| *index);
    let [
        (skip_idx, skip_path, skip),
        (alpha_idx, alpha_path, alpha),
        (beta_idx, beta_path, beta),
    ] = files;

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

    let alpha_out = resolved_file_path(&leecher_dest, &inspected.name, alpha_path);
    let beta_out = resolved_file_path(&leecher_dest, &inspected.name, beta_path);
    let skip_out = resolved_file_path(&leecher_dest, &inspected.name, skip_path);

    assert_eq!(std::fs::read(&alpha_out)?, alpha);
    assert_eq!(std::fs::read(&beta_out)?, beta);
    assert!(
        !skip_out.try_exists()?,
        "an unselected file on separate pieces should not leave a placeholder"
    );

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

const CHECKPOINT_PIECE: usize = 128 * 1024;

async fn reopen_with_saved_bitfield(
    cached: u8,
    seed_payload: &[u8],
    payload_on_disk: &[u8],
    expected_bytes: u64,
    write_partial: impl FnOnce(&Path) -> std::io::Result<()>,
) -> TestResult {
    let temp = TempDir::new()?;
    let seed_file = temp.path().join("checkpoint.bin");
    std::fs::write(&seed_file, seed_payload)?;
    let settings = TorrentSettings::offline_ephemeral();
    let seed = TorrentManager::new(
        temp.path().join("seed-session"),
        temp.path().join("seed"),
        settings.clone(),
    )
    .await?;
    let (torrent, hash, peer) = seed
        .create_and_serve_local(&seed_file, vec![], Some(CHECKPOINT_PIECE as u32))
        .await?;
    let data = temp.path().join("data");
    let destination = temp.path().join("destination");
    let id = DownloadId(614);
    let descriptor = temp.path().join("checkpoint.torrent");
    std::fs::write(&descriptor, &torrent)?;
    let mut request = local_request(id, descriptor, torrent, &destination, peer, None);
    request.initial_peers.clear();
    let cancellation = CancellationToken::new();
    let manager = TorrentManager::new(data.clone(), destination.clone(), settings.clone()).await?;
    manager
        .start_or_resume(request.clone(), &cancellation)
        .await?;
    poll_until(&manager, id, Duration::from_secs(8), |state| {
        matches!(state.phase, TorrentPhase::Downloading)
    })
    .await?;
    manager.shutdown().await;
    drop(manager);

    // Restore the simulated on-disk state in a separate session directory. The first session's
    // background bitfield writer can still drain after shutdown, so it must not own this copy.
    let restored_data = temp.path().join("restored-data");
    std::fs::create_dir(&restored_data)?;
    for relative in ["session.json".to_owned(), format!("{hash}.torrent")] {
        std::fs::copy(data.join(&relative), restored_data.join(relative))?;
    }
    std::fs::write(restored_data.join(format!("{hash}.bitv")), [cached])?;
    let partial = destination.join("checkpoint.bin");
    write_partial(&partial)?;

    let restored = TorrentManager::new(restored_data, destination, settings).await?;
    restored.reconcile(&[(id, Some(hash))]).await;
    restored.start_or_resume(request, &cancellation).await?;
    let snap = poll_until(&restored, id, Duration::from_secs(8), |state| {
        matches!(
            state.phase,
            TorrentPhase::Downloading | TorrentPhase::Seeding
        )
    })
    .await?;
    assert_eq!(snap.downloaded_bytes, expected_bytes);
    assert_eq!(snap.peers_connected, 0);
    assert_eq!(std::fs::read(&partial)?, payload_on_disk);
    restored.shutdown().await;
    seed.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn restores_disjoint_verified_pieces_missing_from_a_stale_bitfield() -> TestResult {
    let seed = deterministic_bytes(161, 4 * CHECKPOINT_PIECE);
    let mut on_disk = seed.clone();
    on_disk[..CHECKPOINT_PIECE].fill(0);
    on_disk[2 * CHECKPOINT_PIECE..3 * CHECKPOINT_PIECE].fill(0);
    reopen_with_saved_bitfield(0, &seed, &on_disk, (2 * CHECKPOINT_PIECE) as u64, |path| {
        std::fs::write(path, &on_disk)
    })
    .await
}

#[tokio::test]
async fn rejects_corrupt_data_claimed_by_a_complete_bitfield() -> TestResult {
    let seed = deterministic_bytes(161, 4 * CHECKPOINT_PIECE);
    let mut on_disk = seed.clone();
    on_disk[0] ^= 0xff;
    reopen_with_saved_bitfield(
        0xf0,
        &seed,
        &on_disk,
        (3 * CHECKPOINT_PIECE) as u64,
        |path| std::fs::write(path, &on_disk),
    )
    .await
}

#[cfg(windows)]
#[tokio::test]
async fn resume_recovers_zero_and_allocated_pieces_in_a_sparse_file() -> TestResult {
    use std::io::{Seek, SeekFrom, Write};

    let mut seed = deterministic_bytes(161, 4 * CHECKPOINT_PIECE);
    seed[2 * CHECKPOINT_PIECE..3 * CHECKPOINT_PIECE].fill(0);
    let mut on_disk = seed.clone();
    on_disk[CHECKPOINT_PIECE..2 * CHECKPOINT_PIECE].fill(0);
    reopen_with_saved_bitfield(0, &seed, &on_disk, (3 * CHECKPOINT_PIECE) as u64, |path| {
        // Initializing the receiver sets the file length with sparse storage on Windows.
        let mut file = std::fs::OpenOptions::new().write(true).open(path)?;
        file.write_all(&on_disk[..CHECKPOINT_PIECE])?;
        file.seek(SeekFrom::Start((3 * CHECKPOINT_PIECE) as u64))?;
        file.write_all(&on_disk[3 * CHECKPOINT_PIECE..])?;
        file.sync_all()
    })
    .await
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
    let error = result.err().ok_or("interface binding should fail")?;
    #[cfg(windows)]
    assert!(matches!(error, TorrentError::InterfaceBindingUnsupported));
    #[cfg(not(windows))]
    assert!(matches!(error, TorrentError::InvalidInterface(_)));

    seeder.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn disables_peer_exchange_without_breaking_extended_handshakes() -> TestResult {
    let temp = TempDir::new()?;
    let source = temp.path().join("sample.bin");
    std::fs::write(&source, deterministic_bytes(32, 64 * 1024))?;
    for (index, enable_pex) in [false, true].into_iter().enumerate() {
        let manager = TorrentManager::new(
            temp.path().join(format!("session{index}")),
            temp.path().join("dest"),
            TorrentSettings {
                enable_pex,
                ..TorrentSettings::offline_ephemeral()
            },
        )
        .await?;
        let (_, hash, addr) = manager
            .create_and_serve_local(&source, vec![], None)
            .await?;
        let handshake = async {
            let mut stream = TcpStream::connect(addr).await?;
            let mut sent = [0_u8; 68];
            sent[0] = 19;
            sent[1..20].copy_from_slice(b"BitTorrent protocol");
            sent[25] = 0x10; // BEP 10 extension bit.
            for (index, value) in sent[28..48].iter_mut().enumerate() {
                *value = u8::from_str_radix(&hash[2 * index..2 * index + 2], 16)?;
            }
            sent[48..].copy_from_slice(b"-QA0001-012345678901");
            stream.write_all(&sent).await?;
            let mut header = [0_u8; 68];
            stream.read_exact(&mut header).await?;
            let size = stream.read_u32().await? as usize;
            assert!((2..2048).contains(&size));
            let mut extended = vec![0_u8; size];
            stream.read_exact(&mut extended).await?;
            assert_eq!(&extended[..2], &[20, 0]);
            assert!(extended.windows(14).any(|bytes| bytes == b"11:ut_metadata"));
            let disabled = extended.windows(11).any(|bytes| bytes == b"6:ut_pexi0e");
            assert_eq!(disabled, !enable_pex);
            Ok::<_, Box<dyn Error>>(())
        };
        tokio::time::timeout(Duration::from_secs(4), handshake).await??;
        manager.shutdown().await;
    }
    Ok(())
}

#[tokio::test]
async fn preferred_port_falls_back_but_fixed_port_fails_without_moving() -> TestResult {
    // Use the session's address family so systems that separate IPv4 and IPv6 binds
    // reliably report a conflict on the listener actually requested by the session.
    #[cfg(not(windows))]
    let occupied = librqbit_dualstack_sockets::TcpListener::bind_tcp(
        (Ipv6Addr::UNSPECIFIED, 0).into(),
        librqbit_dualstack_sockets::BindOpts::default(),
    )?;
    #[cfg(windows)]
    let occupied = bind_exclusive_dual_stack()?;
    #[cfg(not(windows))]
    let port = occupied.bind_addr().port();
    #[cfg(windows)]
    let port = occupied.local_addr()?.port();
    let dir = TempDir::new()?;
    let mut settings = TorrentSettings {
        listen_port: port,
        ..TorrentSettings::offline_ephemeral()
    };
    let fixed = TorrentManager::new(
        dir.path().join("fixed"),
        dir.path().join("dest"),
        settings.clone(),
    )
    .await;
    assert!(
        matches!(fixed, Err(TorrentError::PortUnavailable(actual)) if actual == port),
        "expected an occupied port error, got {fixed:?}"
    );

    settings.fallback_listen_port = true;
    let manager = TorrentManager::new(
        dir.path().join("automatic"),
        dir.path().join("dest"),
        settings,
    )
    .await?;
    let actual = manager
        .listen_port()
        .await
        .ok_or("missing automatic listening port")?;
    assert_ne!(actual, port);
    assert_ne!(actual, 0);
    TcpStream::connect((Ipv4Addr::LOCALHOST, actual)).await?;
    manager.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn udp_port_conflict_keeps_requested_mixed_transport_or_reports_the_fixed_port() -> TestResult
{
    #[cfg(not(windows))]
    let occupied = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).await?;
    #[cfg(windows)]
    let occupied = librqbit_dualstack_sockets::UdpSocket::bind_udp(
        (Ipv6Addr::UNSPECIFIED, 0).into(),
        librqbit_dualstack_sockets::BindOpts {
            request_dualstack: true,
            reuseport: false,
            device: None,
        },
    )?;
    #[cfg(not(windows))]
    let port = occupied.local_addr()?.port();
    #[cfg(windows)]
    let port = occupied.bind_addr().port();
    let dir = TempDir::new()?;
    let mut settings = TorrentSettings {
        listen_port: port,
        enable_utp: true,
        ..TorrentSettings::offline_ephemeral()
    };
    let fixed = TorrentManager::new(
        dir.path().join("fixed"),
        dir.path().join("dest"),
        settings.clone(),
    )
    .await;
    assert!(
        matches!(fixed, Err(TorrentError::PortUnavailable(actual)) if actual == port),
        "expected an occupied UDP port error, got {fixed:?}"
    );

    settings.fallback_listen_port = true;
    let manager = TorrentManager::new(
        dir.path().join("automatic"),
        dir.path().join("dest"),
        settings.clone(),
    )
    .await?;
    let actual = manager
        .listen_port()
        .await
        .ok_or("missing paired listening port")?;
    assert_ne!(actual, port);
    assert_ne!(actual, 0);
    assert_paired_listener(actual).await?;

    // Pinning the negotiated port restarts the session at the same TCP/UDP address.
    settings.listen_port = actual;
    settings.fallback_listen_port = false;
    manager.apply_settings(settings.clone()).await?;
    assert_eq!(manager.listen_port().await, Some(actual));
    assert_paired_listener(actual).await?;

    settings.listen_port = 0;
    manager.apply_settings(settings).await?;
    let automatic = manager
        .listen_port()
        .await
        .ok_or("missing automatic port")?;
    assert_paired_listener(automatic).await?;
    manager.shutdown().await;
    Ok(())
}

async fn assert_paired_listener(port: u16) -> TestResult {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpStream::connect(address).await?;
    let probe = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let mut syn = [0u8; 20];
    syn[0] = 0x41; // BEP 29 ST_SYN, version 1.
    syn[2..4].copy_from_slice(&0x1234_u16.to_be_bytes());
    syn[12..16].copy_from_slice(&(1024_u32 * 1024).to_be_bytes());
    syn[16..18].copy_from_slice(&1_u16.to_be_bytes());
    probe.send_to(&syn, address).await?;
    let mut reply = [0u8; 128];
    let (length, from) =
        tokio::time::timeout(Duration::from_secs(3), probe.recv_from(&mut reply)).await??;
    assert_eq!(from, address);
    assert!(length >= 20);
    assert_eq!(&reply[..4], &[0x21, 0, 0x12, 0x34]);
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)] // Tokio has no API for an exclusive dual-stack Winsock listener.
fn bind_exclusive_dual_stack() -> std::io::Result<TcpListener> {
    use std::{io, mem, net::Ipv6Addr, os::windows::io::AsRawSocket};
    use tokio::net::TcpSocket;
    use windows_sys::Win32::Networking::WinSock::{
        IPPROTO_IPV6, IPV6_V6ONLY, SO_EXCLUSIVEADDRUSE, SOL_SOCKET, WSAGetLastError, setsockopt,
    };
    let socket = TcpSocket::new_v6()?;
    for (level, name, enabled) in [
        (IPPROTO_IPV6, IPV6_V6ONLY, 0_i32),
        (SOL_SOCKET, SO_EXCLUSIVEADDRUSE, 1_i32),
    ] {
        // SAFETY: Tokio owns the live socket. The option value has the integer size documented by Winsock and lives through this synchronous call.
        let result = unsafe {
            setsockopt(
                socket.as_raw_socket() as usize,
                level,
                name,
                (&raw const enabled).cast(),
                mem::size_of_val(&enabled) as i32,
            )
        };
        if result != 0 {
            // SAFETY: Read the calling thread's Winsock error immediately after setsockopt failed.
            return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
        }
    }
    socket.bind((Ipv6Addr::UNSPECIFIED, 0).into())?;
    socket.listen(8)
}

#[tokio::test]
#[ignore = "requires live internet and DHT connectivity"]
async fn live_dht_magnet_inspection() -> TestResult {
    let info_hash = std::env::var("RUSCELLO_LIVE_DHT_INFO_HASH")?;
    assert!(info_hash.len() == 40 && info_hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let dir = TempDir::new()?;
    let settings = TorrentSettings {
        enable_dht: true,
        enable_pex: false,
        dht_bootstrap_addrs: None,
        ..TorrentSettings::offline_ephemeral()
    };
    let manager =
        TorrentManager::new(dir.path().join("data"), dir.path().join("dest"), settings).await?;

    let source = TorrentSource::Magnet(format!("magnet:?xt=urn:btih:{info_hash}"));
    let cancellation = CancellationToken::new();
    let result = tokio::time::timeout(
        Duration::from_secs(90),
        manager.inspect(&source, vec![], &cancellation),
    )
    .await;
    cancellation.cancel();
    manager.shutdown().await;
    let inspection = result??;
    assert!(inspection.info_hash.eq_ignore_ascii_case(&info_hash));
    assert!(!inspection.files.is_empty());
    assert!(inspection.total_bytes > 0);
    Ok(())
}

#[tokio::test]
#[ignore = "requires an isolated µTP-only qBittorrent seeder and a loopback tracker fixture"]
async fn qbittorrent_utp_interoperability_resumes_after_pause_and_restart() -> TestResult {
    let metainfo = PathBuf::from(
        std::env::var_os("RUSCELLO_QA_UTP_TORRENT").ok_or("set RUSCELLO_QA_UTP_TORRENT")?,
    );
    let reference = PathBuf::from(
        std::env::var_os("RUSCELLO_QA_UTP_PAYLOAD").ok_or("set RUSCELLO_QA_UTP_PAYLOAD")?,
    );
    let bytes = std::fs::read(&metainfo)?;
    let inspected = ruscello_torrent::inspect_torrent_bytes(&bytes)?;
    assert_eq!(inspected.files.len(), 1);
    assert_eq!(inspected.total_bytes, 24 * 1024 * 1024);
    assert!(
        inspected
            .trackers
            .iter()
            .all(|tracker| tracker.starts_with("http://127.0.0.1:"))
    );
    let temp = TempDir::new()?;
    let destination = temp.path().join("output");
    let session = temp.path().join("session");
    let settings = TorrentSettings {
        enable_utp: std::env::var_os("RUSCELLO_QA_UTP_TCP_CONTROL").is_none(),
        enable_pex: false,
        download_limit_bps: NonZeroU64::new(2 * 1024 * 1024),
        max_peers_per_torrent: NonZeroUsize::new(2),
        max_global_peers: NonZeroUsize::MIN.saturating_add(2),
        ..TorrentSettings::offline_ephemeral()
    };
    let manager =
        TorrentManager::new(session.clone(), destination.clone(), settings.clone()).await?;
    let cancellation = CancellationToken::new();
    let id = DownloadId(77);
    let mut request = local_request(
        id,
        metainfo,
        bytes,
        &destination,
        (Ipv4Addr::LOCALHOST, 0).into(),
        None,
    );
    request.initial_peers.clear();
    let started = manager
        .start_or_resume(request.clone(), &cancellation)
        .await?;
    poll_until(&manager, id, Duration::from_secs(20), |state| {
        state.downloaded_bytes >= 3 * 1024 * 1024
    })
    .await?;
    let expected_transport = if settings.enable_utp {
        TorrentPeerTransport::Utp
    } else {
        TorrentPeerTransport::Tcp
    };
    assert!(
        manager
            .details(id)
            .await?
            .peers
            .iter()
            .any(|peer| peer.transport == Some(expected_transport)),
        "the fixture did not use {expected_transport:?}"
    );
    manager.pause(id).await?;
    let paused_at = manager.poll_progress(id)?.downloaded_bytes;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(manager.poll_progress(id)?.downloaded_bytes, paused_at);
    let resume_time = Instant::now();
    manager
        .start_or_resume(request.clone(), &cancellation)
        .await?;
    poll_until(&manager, id, Duration::from_secs(90), |state| {
        state.downloaded_bytes >= 8 * 1024 * 1024
    })
    .await?;
    eprintln!("Time from resume to 8 MiB: {:?}", resume_time.elapsed());

    manager.shutdown().await;
    drop(manager);
    let restart_time = Instant::now();
    let restored = TorrentManager::new(session, destination, settings).await?;
    restored
        .reconcile(&[(id, Some(started.info_hash.clone()))])
        .await;
    restored.start_or_resume(request, &cancellation).await?;
    poll_until(&restored, id, Duration::from_secs(90), |state| {
        matches!(state.phase, TorrentPhase::Seeding)
    })
    .await?;
    eprintln!(
        "Time from shutdown/restart to verified completion: {:?}",
        restart_time.elapsed()
    );
    assert_eq!(
        std::fs::read(started.completed_path)?,
        std::fs::read(reference)?
    );
    restored.shutdown().await;
    Ok(())
}
