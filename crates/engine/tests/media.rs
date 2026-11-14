use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use ruscello_core::{DownloadId, FileType, Source, Status};
use ruscello_engine::{
    AddDownload, EngineEvent, EngineHandle, EngineOptions, MediaKind, MediaRequest,
    SidecarSettings, Url,
};
use ruscello_testkit::{FileSpec, Reply, TestServer};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn create_mock_ffmpeg(dir: &Path) -> TestResult<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join("mock-ffmpeg.sh");
        let script = "#!/bin/sh\n\
            if [ \"$1\" = \"-version\" ]; then\n\
              echo \"ffmpeg version 7.1-mock Copyright\"\n\
              exit 0\n\
            fi\n\
            v=\"\"; a=\"\"; out=\"\"\n\
            while [ \"$#\" -gt 0 ]; do\n\
              if [ \"$1\" = \"-i\" ]; then\n\
                shift\n\
                if [ -z \"$v\" ]; then v=\"$1\"; else a=\"$1\"; fi\n\
              else\n\
                out=\"$1\"\n\
              fi\n\
              shift\n\
            done\n\
            sleep 0.35\n\
            cat \"$v\" \"$a\" > \"$out\"\n";
        fs::write(&path, script)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        Ok(path)
    }
    #[cfg(windows)]
    {
        let path = dir.join("mock-ffmpeg.cmd");
        let script = "@echo off\r\n\
            if \"%1\"==\"-version\" (\r\n\
              echo ffmpeg version 7.1-mock Copyright\r\n\
              exit /b 0\r\n\
            )\r\n\
            set v=\r\n\
            set a=\r\n\
            set out=\r\n\
            :loop\r\n\
            if \"%1\"==\"\" goto done\r\n\
            if \"%1\"==\"-i\" (\r\n\
              if \"%v%\"==\"\" (set v=%~2) else (set a=%~2)\r\n\
              shift\r\n\
              shift\r\n\
              goto loop\r\n\
            )\r\n\
            set out=%~1\r\n\
            shift\r\n\
            goto loop\r\n\
            :done\r\n\
            ping -n 2 127.0.0.1 >nul\r\n\
            copy /b \"%v%\"+\"%a%\" \"%out%\" >nul\r\n";
        fs::write(&path, script)?;
        Ok(path)
    }
}

async fn wait_for_status(
    events: &mut tokio::sync::broadcast::Receiver<EngineEvent>,
    target_id: DownloadId,
    target_status: Status,
) -> TestResult {
    timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await {
                Ok(EngineEvent::StatusChanged { id, status })
                    if id == target_id && status == target_status =>
                {
                    return Ok(());
                }
                Ok(EngineEvent::StatusChanged { id, status })
                    if id == target_id && matches!(status, Status::Failed { .. }) =>
                {
                    return Err(format!("download failed unexpectedly: {status:?}").into());
                }
                Ok(_) => {}
                Err(error) => return Err(Box::new(error) as Box<dyn Error>),
            }
        }
    })
    .await?
}

#[tokio::test]
async fn engine_inspects_and_downloads_hls_stream_and_persists_sidecar_settings() -> TestResult {
    let server = TestServer::start().await?;
    let temp = tempfile::tempdir()?;
    let dest = temp.path().join("downloads");
    fs::create_dir_all(&dest)?;

    let seg0 = b"HLS_SEG_ZERO|";
    let seg1 = b"HLS_SEG_ONE";
    server.route("/hls/720/0.ts", move |_| {
        Reply::bytes("video/mp2t", seg0.to_vec())
    });
    server.route("/hls/720/1.ts", move |_| {
        Reply::bytes("video/mp2t", seg1.to_vec())
    });
    server.route("/hls/720.m3u8", |_| {
        let playlist = "\
#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:4
#EXTINF:4.0,
/hls/720/0.ts
#EXTINF:4.0,
/hls/720/1.ts
#EXT-X-ENDLIST
";
        Reply::bytes(
            "application/vnd.apple.mpegurl",
            playlist.as_bytes().to_vec(),
        )
    });
    let master_url = server.route("/hls/demo.m3u8", |_| {
        let master = "\
#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=2000000,RESOLUTION=1280x720
/hls/720.m3u8
";
        Reply::bytes("application/vnd.apple.mpegurl", master.as_bytes().to_vec())
    });

    let db_path = temp.path().join("ruscello.db");
    let engine = EngineHandle::start(EngineOptions::new(db_path.clone(), dest.clone())).await?;
    let mut events = engine.subscribe();

    let parsed = Url::parse(&master_url)?;
    let inspected = engine
        .inspect_media(&parsed, &CancellationToken::new())
        .await?;
    assert_eq!(inspected.kind, MediaKind::Hls);

    let mut req = AddDownload::new(Source::Media {
        url: master_url.clone(),
    });
    req.filename_hint = Some("Engine Demo".to_owned());
    req.media = Some(MediaRequest {
        kind: MediaKind::Hls,
        format_selector: Some("720p".to_owned()),
        title: Some("Engine Demo".to_owned()),
        ..MediaRequest::default()
    });
    let id = engine.add(req).await?;

    wait_for_status(&mut events, id, Status::Completed).await?;
    let snapshot = engine.snapshot().await?;
    let dl = snapshot
        .downloads
        .iter()
        .find(|d| d.id == id)
        .ok_or("missing completed download")?;
    assert_eq!(dl.file_type, Some(FileType::Video));
    let completed = dl.completed_path.as_ref().ok_or("missing completed_path")?;
    assert_eq!(fs::read(completed)?, b"HLS_SEG_ZERO|HLS_SEG_ONE");

    // Persist sidecar settings and verify across restart.
    let updated = SidecarSettings {
        ytdlp_consent: true,
        ytdlp_auto_update: true,
        ..SidecarSettings::default()
    };
    engine.set_sidecar_settings(updated.clone()).await?;
    engine.shutdown().await?;

    let reopened = EngineHandle::start(EngineOptions::new(db_path, dest)).await?;
    let reopened_snap = reopened.snapshot().await?;
    assert!(reopened_snap.sidecars.ytdlp_consent);
    assert!(reopened_snap.sidecars.ytdlp_auto_update);
    reopened.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn engine_pauses_and_resumes_media_download_preserving_completed_segments() -> TestResult {
    let server = TestServer::start().await?;
    let temp = tempfile::tempdir()?;
    let dest = temp.path().join("downloads");
    fs::create_dir_all(&dest)?;

    let seg0_hits = Arc::new(AtomicUsize::new(0));
    let seg0_counter = seg0_hits.clone();
    let seg0 = vec![b'A'; 4096];
    let seg0_clone = seg0.clone();
    server.route("/resume/0.ts", move |_| {
        seg0_counter.fetch_add(1, Ordering::Relaxed);
        Reply::bytes("video/mp2t", seg0_clone.clone())
    });

    // Throttled TestFile triggers progress & stays open long enough for deterministic pause().
    let seg1_file = server.file(
        FileSpec::random(7, 12 * 1024)
            .filename("seg1.ts")
            .throttle_per_conn(4096),
    );
    let seg1_url = seg1_file.url().to_owned();
    let seg1_expected = seg1_file.payload().bytes(0..12 * 1024);

    let playlist_url = server.route("/resume/stream.m3u8", move |_| {
        let playlist = format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.0,\n/resume/0.ts\n#EXTINF:4.0,\n{seg1_url}\n#EXT-X-ENDLIST\n"
        );
        Reply::bytes("application/vnd.apple.mpegurl", playlist.into_bytes())
    });

    let engine =
        EngineHandle::start(EngineOptions::new(temp.path().join("db.sqlite"), dest)).await?;
    let mut events = engine.subscribe();

    let mut req = AddDownload::new(Source::Media { url: playlist_url });
    req.max_connections = Some(std::num::NonZeroUsize::MIN);
    req.media = Some(MediaRequest {
        kind: MediaKind::Hls,
        title: Some("Resumable Stream".to_owned()),
        ..MediaRequest::default()
    });
    let id = engine.add(req).await?;

    // Wait until segment 0 is written and segment 1 starts streaming bytes.
    timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await {
                Ok(EngineEvent::Progress(updates))
                    if updates
                        .iter()
                        .any(|u| u.id == id && u.progress.completed >= 4096) =>
                {
                    return Ok::<_, Box<dyn Error>>(());
                }
                Ok(_) => {}
                Err(error) => return Err(Box::new(error) as Box<dyn Error>),
            }
        }
    })
    .await??;

    engine.pause(id).await?;
    let paused = engine
        .snapshot()
        .await?
        .downloads
        .into_iter()
        .find(|d| d.id == id)
        .ok_or("missing paused download")?;
    assert_eq!(paused.status(), Status::Paused);
    assert_eq!(seg0_hits.load(Ordering::Relaxed), 1);

    // Clear throttle isn't needed (32KB at 4KB/s? Let's make seg1 complete faster or wait up to 15s? Wait 32KB/4KB = 8s -> within 15s timeout).
    engine.resume(id).await?;
    timeout(Duration::from_secs(15), async {
        loop {
            match events.recv().await {
                Ok(EngineEvent::StatusChanged {
                    id: ev_id,
                    status: Status::Completed,
                }) if ev_id == id => return Ok::<_, Box<dyn Error>>(()),
                Ok(_) => {}
                Err(error) => return Err(Box::new(error) as Box<dyn Error>),
            }
        }
    })
    .await??;

    assert_eq!(seg0_hits.load(Ordering::Relaxed), 1);
    let completed = engine
        .snapshot()
        .await?
        .downloads
        .into_iter()
        .find(|d| d.id == id)
        .and_then(|d| d.completed_path)
        .ok_or("missing final file")?;
    let actual = fs::read(completed)?;
    let mut expected = seg0;
    expected.extend_from_slice(&seg1_expected);
    assert_eq!(actual, expected);
    engine.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn engine_tracks_post_processing_during_dash_mux_and_groups_playlists_into_package()
-> TestResult {
    let server = TestServer::start().await?;
    let temp = tempfile::tempdir()?;
    let dest = temp.path().join("downloads");
    fs::create_dir_all(&dest)?;

    server.route("/dash/v.bin", |_| {
        Reply::bytes("video/mp4", b"VIDEO|".to_vec())
    });
    server.route("/dash/a.bin", |_| {
        Reply::bytes("audio/mp4", b"AUDIO".to_vec())
    });
    let mpd_url = server.route("/dash/mux.mpd", |_| {
        let mpd = r#"<?xml version="1.0" encoding="utf-8"?>
<MPD type="static" mediaPresentationDuration="PT2.0S">
  <Period>
    <AdaptationSet mimeType="video/mp4" contentType="video">
      <Representation id="v1" bandwidth="1000000" width="1280" height="720">
        <SegmentList><SegmentURL media="/dash/v.bin"/></SegmentList>
      </Representation>
    </AdaptationSet>
    <AdaptationSet mimeType="audio/mp4" contentType="audio">
      <Representation id="a1" bandwidth="128000">
        <SegmentList><SegmentURL media="/dash/a.bin"/></SegmentList>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;
        Reply::bytes("application/dash+xml", mpd.as_bytes().to_vec())
    });

    let ep1_url = server.route("/pl/ep1.m3u8", |_| {
        Reply::bytes(
            "application/vnd.apple.mpegurl",
            b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXTINF:2.0,\n/dash/v.bin\n#EXT-X-ENDLIST\n".to_vec(),
        )
    });
    let ep2_url = server.route("/pl/ep2.m3u8", |_| {
        Reply::bytes(
            "application/vnd.apple.mpegurl",
            b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXTINF:2.0,\n/dash/a.bin\n#EXT-X-ENDLIST\n".to_vec(),
        )
    });

    let mock_ff = create_mock_ffmpeg(temp.path())?;
    let mut options = EngineOptions::new(temp.path().join("db.sqlite"), dest);
    options.sidecars = SidecarSettings {
        ffmpeg_consent: true,
        ffmpeg_custom_path: Some(mock_ff),
        ..SidecarSettings::default()
    };
    let engine = EngineHandle::start(options).await?;
    let mut events = engine.subscribe();

    let mut dash_req = AddDownload::new(Source::Media { url: mpd_url });
    dash_req.media = Some(MediaRequest {
        kind: MediaKind::Dash,
        format_selector: Some("best".to_owned()),
        title: Some("Muxed Movie".to_owned()),
        ..MediaRequest::default()
    });

    let dash_id = engine.add(dash_req).await?;
    let saw_post_proc = Arc::new(AtomicBool::new(false));
    timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await {
                Ok(EngineEvent::StatusChanged {
                    id,
                    status: Status::PostProcessing,
                }) if id == dash_id => {
                    saw_post_proc.store(true, Ordering::Release);
                }
                Ok(EngineEvent::StatusChanged {
                    id,
                    status: Status::Completed,
                }) if id == dash_id => break,
                Ok(EngineEvent::StatusChanged {
                    id,
                    status: Status::Failed { reason },
                }) if id == dash_id => {
                    return Err(format!("DASH download failed: {reason:?}").into());
                }
                Ok(_) => {}
                Err(error) => return Err(Box::new(error) as Box<dyn Error>),
            }
        }
        Ok::<_, Box<dyn Error>>(())
    })
    .await??;

    assert!(saw_post_proc.load(Ordering::Acquire));

    // Group playlist episodes into a named Package.
    let mut item1 = AddDownload::new(Source::Media { url: ep1_url });
    item1.media = Some(MediaRequest {
        kind: MediaKind::Hls,
        title: Some("Episode 1".to_owned()),
        ..MediaRequest::default()
    });
    let mut item2 = AddDownload::new(Source::Media { url: ep2_url });
    item2.media = Some(MediaRequest {
        kind: MediaKind::Hls,
        title: Some("Episode 2".to_owned()),
        ..MediaRequest::default()
    });
    let group_ids = engine
        .add_groups(vec![(
            Some("Playlist Alpha".to_owned()),
            vec![item1, item2],
        )])
        .await?;
    assert_eq!(group_ids.len(), 2);

    let snap = engine.snapshot().await?;
    let pkg = snap
        .packages
        .iter()
        .find(|p| p.name == "Playlist Alpha")
        .ok_or("missing Playlist Alpha package")?;
    for id in group_ids {
        let dl = snap
            .downloads
            .iter()
            .find(|d| d.id == id)
            .ok_or("missing grouped download")?;
        assert_eq!(dl.package, Some(pkg.id));
    }

    engine.shutdown().await?;
    Ok(())
}
