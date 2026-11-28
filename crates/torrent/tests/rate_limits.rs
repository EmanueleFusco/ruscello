use std::{num::NonZeroU32, time::Duration};

use librqbit::{
    AddTorrent, AddTorrentOptions, CreateTorrentOptions, ListenerMode, ListenerOptions, Session,
    SessionOptions,
    limits::{Limits, LimitsConfig},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn changing_a_small_cap_wakes_a_pending_block() -> TestResult {
    let limits = Limits::new(LimitsConfig {
        download_bps: Some(NonZeroU32::MIN),
        ..LimitsConfig::default()
    });
    let block = NonZeroU32::new(16 * 1024).ok_or("empty test block")?;
    let pending = limits.prepare_for_download(block);
    tokio::pin!(pending);
    let waiting =
        std::future::poll_fn(|cx| std::task::Poll::Ready(pending.as_mut().poll(cx).is_pending()))
            .await;
    assert!(waiting);
    limits.set_download_bps(None);
    tokio::time::timeout(Duration::from_secs(2), pending).await??;
    Ok(())
}

#[derive(Clone, Copy)]
enum Incoming {
    Idle,
    Active(Option<NonZeroU32>),
}

async fn capped_transfer(mode: ListenerMode, incoming: Incoming) -> TestResult {
    const UPLOAD_BPS: u32 = 8 * 1024;
    let root = tempfile::tempdir()?;
    let capped = root.path().join("capped");
    let remote = root.path().join("remote");
    std::fs::create_dir_all(&capped)?;
    std::fs::create_dir_all(&remote)?;
    let options = |upload_bps, download_bps| SessionOptions {
        listen: Some(ListenerOptions {
            listen_addr: (std::net::Ipv4Addr::LOCALHOST, 0).into(),
            mode,
            ..Default::default()
        }),
        dht: None,
        disable_local_service_discovery: true,
        ratelimits: LimitsConfig {
            upload_bps,
            download_bps,
        },
        ..Default::default()
    };
    let sender = Session::new_with_opts(
        capped.clone(),
        options(
            NonZeroU32::new(UPLOAD_BPS),
            match incoming {
                Incoming::Idle => None,
                Incoming::Active(cap) => cap,
            },
        ),
    )
    .await?;
    let remote_session =
        Session::new_with_opts(remote.clone(), options(NonZeroU32::new(32 * 1024), None)).await?;
    let sender_addr = sender.listen_addr().ok_or("no sender address")?;
    let remote_addr = remote_session.listen_addr().ok_or("no remote address")?;
    let create = || CreateTorrentOptions {
        name: None,
        trackers: vec![],
        piece_length: Some(32 * 1024),
    };
    let add = |peer| {
        Some(AddTorrentOptions {
            initial_peers: Some(vec![peer]),
            disable_trackers: true,
            ..Default::default()
        })
    };
    let outgoing_name = "outgoing.bin";
    let outgoing = vec![123; 32 * 1024];
    std::fs::write(capped.join(outgoing_name), &outgoing)?;
    let (metadata, source) = sender
        .create_and_serve_torrent(&capped.join(outgoing_name), create())
        .await?;
    source.wait_until_completed().await?;
    let mut inbound = None;
    if matches!(incoming, Incoming::Active(_)) {
        let incoming_name = "incoming.bin";
        let incoming = vec![15; 192 * 1024];
        std::fs::write(remote.join(incoming_name), &incoming)?;
        let (metadata, incoming_source) = remote_session
            .create_and_serve_torrent(&remote.join(incoming_name), create())
            .await?;
        incoming_source.wait_until_completed().await?;
        let destination = sender
            .add_torrent(
                AddTorrent::TorrentFileBytes(metadata.as_bytes()?),
                add(remote_addr),
            )
            .await?
            .into_handle()
            .ok_or("no inbound transfer")?;
        inbound = Some((destination, incoming_name, incoming));
    }
    let start = std::time::Instant::now();
    let destination = remote_session
        .add_torrent(
            AddTorrent::TorrentFileBytes(metadata.as_bytes()?),
            add(sender_addr),
        )
        .await?
        .into_handle()
        .ok_or("no outbound transfer")?;
    tokio::time::timeout(Duration::from_secs(15), destination.wait_until_completed()).await??;
    assert_eq!(source.stats().uploaded_bytes, outgoing.len() as u64);
    assert_eq!(std::fs::read(remote.join(outgoing_name))?, outgoing);
    // A fresh bucket permits one second of burst, so 32 KiB at 8 KiB/s takes at least three seconds.
    assert!(start.elapsed() >= Duration::from_millis(2850));
    if let Some((destination, incoming_name, incoming)) = inbound {
        tokio::time::timeout(Duration::from_secs(15), destination.wait_until_completed()).await??;
        assert_eq!(std::fs::read(capped.join(incoming_name))?, incoming);
    }
    sender.stop().await;
    remote_session.stop().await;
    Ok(())
}

#[tokio::test]
async fn tcp_upload_cap_works_while_seeding_and_with_downloads() -> TestResult {
    tokio::try_join!(
        capped_transfer(ListenerMode::TcpOnly, Incoming::Idle),
        capped_transfer(ListenerMode::TcpOnly, Incoming::Active(None)),
        capped_transfer(
            ListenerMode::TcpOnly,
            Incoming::Active(NonZeroU32::new(24 * 1024))
        ),
    )?;
    Ok(())
}

#[tokio::test]
async fn utp_upload_cap_works_while_seeding_and_with_downloads() -> TestResult {
    tokio::try_join!(
        capped_transfer(ListenerMode::UtpOnly, Incoming::Idle),
        capped_transfer(ListenerMode::UtpOnly, Incoming::Active(None)),
        capped_transfer(
            ListenerMode::UtpOnly,
            Incoming::Active(NonZeroU32::new(24 * 1024))
        ),
    )?;
    Ok(())
}
