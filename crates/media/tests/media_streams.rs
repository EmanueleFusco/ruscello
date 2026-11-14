use std::{
    error::Error,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use aes::cipher::{BlockModeEncrypt as _, KeyIvInit as _, block_padding::Pkcs7};
use flate2::{Compression, write::GzEncoder};
use reqwest::{
    StatusCode, Url,
    header::{COOKIE, HeaderMap, HeaderValue},
};
use ruscello_http::{DownloadProgress, HttpDownloader};
use ruscello_media::{
    BandwidthContext, MediaEngine, MediaError, MediaKind, MediaPhase, MediaRequest,
    MediaTransferJob, SidecarEndpoints, SidecarKind, SidecarSettings, parse_ytdlp_json,
};
use ruscello_testkit::{FileSpec, Reply, Request, TestServer};
use sha2::{Digest as _, Sha256};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn encrypt_aes128(key: &[u8; 16], iv: &[u8; 16], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let padded_len = ((plaintext.len() / 16) + 1) * 16;
    let mut buf = vec![0_u8; padded_len];
    buf[..plaintext.len()].copy_from_slice(plaintext);
    let cipher = cbc::Encryptor::<aes::Aes128>::new_from_slices(key, iv)
        .map_err(|error| format!("cipher init failed: {error}"))?;
    let encrypted = cipher
        .encrypt_padded::<Pkcs7>(&mut buf, plaintext.len())
        .map_err(|error| format!("encrypt failed: {error}"))?;
    Ok(encrypted.to_vec())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn make_tar_gz(binary_name: &str, contents: &[u8]) -> TestResult<Vec<u8>> {
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, format!("bin/{binary_name}"), contents)?;
        builder.finish()?;
    }
    let mut gz = GzEncoder::new(Vec::new(), Compression::fast());
    gz.write_all(&tar_bytes)?;
    Ok(gz.finish()?)
}

fn make_zip(binary_name: &str, contents: &[u8]) -> TestResult<Vec<u8>> {
    let cursor = std::io::Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(cursor);
    writer.start_file(
        format!("bin/{binary_name}"),
        zip::write::SimpleFileOptions::default(),
    )?;
    writer.write_all(contents)?;
    let finished = writer.finish()?;
    Ok(finished.into_inner())
}

fn create_mock_ffmpeg(dir: &Path) -> TestResult<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join("mock-ffmpeg.sh");
        // Records CLI arguments then concatenates both -i inputs into the last output argument.
        let log_path = dir.join("ffmpeg-args.log");
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"-version\" ]; then\n\
               echo \"ffmpeg version 7.1-mock Copyright\"\n\
               exit 0\n\
             fi\n\
             printf '%s\\n' \"$@\" > \"{}\"\n\
             v=\"\"\n\
             a=\"\"\n\
             out=\"\"\n\
             while [ \"$#\" -gt 0 ]; do\n\
               if [ \"$1\" = \"-i\" ]; then\n\
                 shift\n\
                 if [ -z \"$v\" ]; then v=\"$1\"; else a=\"$1\"; fi\n\
               else\n\
                 out=\"$1\"\n\
               fi\n\
               shift\n\
             done\n\
             printf 'MUX:' > \"$out\"\n\
             cat \"$v\" \"$a\" >> \"$out\"\n",
            log_path.display()
        );
        fs::write(&path, script)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        Ok(path)
    }
    #[cfg(windows)]
    {
        let path = dir.join("mock-ffmpeg.cmd");
        let log_path = dir.join("ffmpeg-args.log");
        let script = format!(
            "@echo off\r\n\
             if \"%1\"==\"-version\" (\r\n\
               echo ffmpeg version 7.1-mock Copyright\r\n\
               exit /b 0\r\n\
             )\r\n\
             echo %* > \"{}\"\r\n\
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
             copy /b /y \"%v%\"+\"%a%\" \"%out%\" >nul\r\n",
            log_path.display()
        );
        fs::write(&path, script)?;
        Ok(path)
    }
}

fn create_mock_ytdlp(
    dir: &Path,
    info_json_path: &Path,
    sub_out_bytes: &[u8],
    cookie_capture: &Path,
) -> TestResult<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join("mock-yt-dlp.sh");
        let sub_data_path = dir.join("mock-sub-output.bin");
        fs::write(&sub_data_path, sub_out_bytes)?;
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then\n\
               echo \"2026.01.01\"\n\
               exit 0\n\
             fi\n\
             is_json=0\n\
             out_tmpl=\"\"\n\
             cookies_file=\"\"\n\
             while [ \"$#\" -gt 0 ]; do\n\
               case \"$1\" in\n\
                 -J) is_json=1 ;;\n\
                 -o) shift; out_tmpl=\"$1\" ;;\n\
                 --cookies) shift; cookies_file=\"$1\" ;;\n\
               esac\n\
               shift\n\
             done\n\
             if [ -n \"$cookies_file\" ] && [ -f \"$cookies_file\" ]; then\n\
               perms=$(stat -c '%a' \"$cookies_file\" 2>/dev/null || stat -f '%Lp' \"$cookies_file\")\n\
               printf 'PERMS=%s\\n' \"$perms\" > \"{}\"\n\
               cat \"$cookies_file\" >> \"{}\"\n\
             fi\n\
             if [ \"$is_json\" -eq 1 ]; then\n\
               cat \"{}\"\n\
               exit 0\n\
             fi\n\
             echo \"RUSCELLO|50|100|100\"\n\
             echo \"RUSCELLO|100|100|100\"\n\
             target=$(printf '%s' \"$out_tmpl\" | sed 's/%(ext)s/mp3/')\n\
             cp \"{}\" \"$target\"\n\
             echo \"$target\"\n",
            cookie_capture.display(),
            cookie_capture.display(),
            info_json_path.display(),
            sub_data_path.display()
        );
        fs::write(&path, script)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        Ok(path)
    }
    #[cfg(windows)]
    {
        let path = dir.join("mock-yt-dlp.cmd");
        let sub_data_path = dir.join("mock-sub-output.bin");
        fs::write(&sub_data_path, sub_out_bytes)?;
        let script = format!(
            "@echo off\r\n\
             if \"%1\"==\"--version\" (\r\n\
               echo 2026.01.01\r\n\
               exit /b 0\r\n\
             )\r\n\
             setlocal enabledelayedexpansion\r\n\
             set is_json=0\r\n\
             set out_tmpl=\r\n\
             :loop\r\n\
             if \"%1\"==\"\" goto run\r\n\
             if \"%1\"==\"-J\" set is_json=1\r\n\
             if \"%1\"==\"-o\" (\r\n\
               set out_tmpl=%~2\r\n\
               shift\r\n\
             )\r\n\
             if \"%1\"==\"--cookies\" (\r\n\
               copy /y \"%~2\" \"{}\" >nul\r\n\
               shift\r\n\
             )\r\n\
             shift\r\n\
             goto loop\r\n\
             :run\r\n\
             if \"%is_json%\"==\"1\" (\r\n\
               type \"{}\"\r\n\
               exit /b 0\r\n\
             )\r\n\
             echo RUSCELLO^|50^|100^|100\r\n\
             echo RUSCELLO^|100^|100^|100\r\n\
             set target=!out_tmpl:%%(ext)s=mp3!\r\n\
             copy /y \"{}\" \"!target!\" >nul\r\n\
             echo !target!\r\n",
            cookie_capture.display(),
            info_json_path.display(),
            sub_data_path.display()
        );
        fs::write(&path, script)?;
        Ok(path)
    }
}

#[tokio::test]
async fn hls_master_quality_selection_aes128_decryption_and_segment_resume() -> TestResult {
    let server = TestServer::start().await?;
    let temp = tempfile::tempdir()?;

    let key: [u8; 16] = *b"ruscello-aes-key";
    let iv: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

    let plain_segments: Vec<Vec<u8>> = (0..4)
        .map(|i| format!("TS-PKT-{i}-{}", "X".repeat(256)).into_bytes())
        .collect();
    let enc_segments: Vec<Vec<u8>> = plain_segments
        .iter()
        .map(|plain| encrypt_aes128(&key, &iv, plain))
        .collect::<Result<_, _>>()
        .map_err(|e| format!("aes setup failed: {e}"))?;

    let observed_cookie = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
    let cookie_ref = observed_cookie.clone();
    let key_hits = Arc::new(AtomicUsize::new(0));
    let key_hits_ref = key_hits.clone();
    server.route("/hls/key.bin", move |req: Request| {
        key_hits_ref.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut guard) = cookie_ref.lock() {
            guard.push(
                req.headers
                    .get("cookie")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned),
            );
        }
        Reply::bytes("application/octet-stream", key.to_vec())
    });

    let fail_seg2_once = Arc::new(AtomicBool::new(true));
    let seg_hits: Vec<Arc<AtomicUsize>> = (0..4).map(|_| Arc::new(AtomicUsize::new(0))).collect();

    for (idx, cipher) in enc_segments.into_iter().enumerate() {
        let hit_counter = seg_hits[idx].clone();
        let fail_flag = fail_seg2_once.clone();
        server.route(&format!("/hls/720/{idx}.ts"), move |_| {
            if idx == 2 && fail_flag.swap(false, Ordering::AcqRel) {
                return Reply::status(StatusCode::FORBIDDEN);
            }
            hit_counter.fetch_add(1, Ordering::Relaxed);
            Reply::bytes("video/mp2t", cipher.clone())
        });
    }

    let med_playlist = "\
#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:6
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-KEY:METHOD=AES-128,URI=\"/hls/key.bin\",IV=0x000102030405060708090a0b0c0d0e0f
#EXTINF:5.0,
/hls/720/0.ts
#EXTINF:5.0,
/hls/720/1.ts
#EXTINF:5.0,
/hls/720/2.ts
#EXTINF:5.0,
/hls/720/3.ts
#EXT-X-ENDLIST
";
    let high_playlist = "\
#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:6
#EXT-X-MEDIA-SEQUENCE:0
#EXTINF:5.0,
/hls/1080/0.ts
#EXT-X-ENDLIST
";
    server.route("/hls/720.m3u8", move |_| {
        Reply::bytes(
            "application/vnd.apple.mpegurl",
            med_playlist.as_bytes().to_vec(),
        )
    });
    server.route("/hls/1080.m3u8", move |_| {
        Reply::bytes(
            "application/vnd.apple.mpegurl",
            high_playlist.as_bytes().to_vec(),
        )
    });
    let master_url = server.route("/hls/master.m3u8", |_| {
        let master = "\
#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,CODECS=\"avc1.640028,mp4a.40.2\"
/hls/1080.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2200000,RESOLUTION=1280x720,CODECS=\"avc1.4d401f,mp4a.40.2\"
/hls/720.m3u8
";
        Reply::bytes("application/vnd.apple.mpegurl", master.as_bytes().to_vec())
    });

    let master = Url::parse(&master_url)?;
    let http = HttpDownloader::new()?;
    let engine = MediaEngine::new(http, temp.path(), SidecarEndpoints::default())?;

    let mut headers = HeaderMap::new();
    headers.insert(COOKIE, HeaderValue::from_static("session=secret123"));
    let settings = SidecarSettings::default();
    let cancel = CancellationToken::new();

    let inspection = engine
        .inspect(&master, &headers, &settings, &cancel)
        .await?;
    assert_eq!(inspection.kind, MediaKind::Hls);
    assert!(inspection.formats.iter().any(|f| f.height == Some(1080)));
    assert!(inspection.formats.iter().any(|f| f.height == Some(720)));

    let working = temp.path().join("work");
    let ready = temp.path().join("ready");
    let bw = BandwidthContext::default();
    let (prog_tx, _prog_rx) = watch::channel(DownloadProgress::default());

    let request = MediaRequest {
        kind: MediaKind::Hls,
        format_selector: Some("720p".to_owned()),
        title: Some("Demo Stream".to_owned()),
        ..MediaRequest::default()
    };

    // First run fails at segment 2 with 403 while concurrency = 1 keeps segments 0 and 1 committed.
    let err = engine
        .download(MediaTransferJob {
            url: &master,
            request: &request,
            session_headers: &headers,
            working_dir: &working,
            ready_dir: &ready,
            filename_hint: Some("Demo Stream"),
            concurrency: 1,
            bandwidth: &bw,
            settings: &settings,
            cancellation: &cancel,
            progress: Some(&prog_tx),
            on_phase: &|_| {},
        })
        .await
        .err()
        .ok_or("expected first run to fail at segment 2")?;
    assert!(matches!(err, MediaError::HttpStatus(403)));
    assert!(working.join("hls/video/segments.ctl").is_file());
    assert_eq!(seg_hits[0].load(Ordering::Relaxed), 1);
    assert_eq!(seg_hits[1].load(Ordering::Relaxed), 1);
    assert_eq!(seg_hits[2].load(Ordering::Relaxed), 0);

    // Resume on same working_dir: completed segments 0 and 1 MUST NOT be re-downloaded.
    let output_path = engine
        .download(MediaTransferJob {
            url: &master,
            request: &request,
            session_headers: &headers,
            working_dir: &working,
            ready_dir: &ready,
            filename_hint: Some("Demo Stream"),
            concurrency: 2,
            bandwidth: &bw,
            settings: &settings,
            cancellation: &cancel,
            progress: Some(&prog_tx),
            on_phase: &|_| {},
        })
        .await?;

    assert_eq!(seg_hits[0].load(Ordering::Relaxed), 1);
    assert_eq!(seg_hits[1].load(Ordering::Relaxed), 1);
    assert_eq!(seg_hits[2].load(Ordering::Relaxed), 1);
    assert_eq!(seg_hits[3].load(Ordering::Relaxed), 1);
    assert!(!working.join("hls/video/segments.ctl").exists());
    assert!(key_hits.load(Ordering::Relaxed) >= 1);

    let expected_plain: Vec<u8> = plain_segments.concat();
    let actual_plain = fs::read(&output_path)?;
    assert_eq!(actual_plain, expected_plain);

    let cookies = observed_cookie.lock().map_err(|e| format!("{e}"))?;
    assert!(
        cookies
            .iter()
            .any(|c| c.as_deref() == Some("session=secret123"))
    );
    Ok(())
}

#[tokio::test]
async fn dash_mpd_segment_template_and_segment_list_mux_and_video_fallback() -> TestResult {
    let server = TestServer::start().await?;
    let temp = tempfile::tempdir()?;

    let init_v = b"INIT_V|";
    let seg_v1 = b"VSEG1|";
    let seg_v2 = b"VSEG2|";
    let init_a = b"INIT_A|";
    let seg_a1 = b"ASEG1|";
    let seg_a2 = b"ASEG2";

    server.route("/dash/v720/init.mp4", move |_| {
        Reply::bytes("video/mp4", init_v.to_vec())
    });
    server.route("/dash/v720/1.m4s", move |_| {
        Reply::bytes("video/mp4", seg_v1.to_vec())
    });
    server.route("/dash/v720/2.m4s", move |_| {
        Reply::bytes("video/mp4", seg_v2.to_vec())
    });
    server.route("/dash/a/init.mp4", move |_| {
        Reply::bytes("audio/mp4", init_a.to_vec())
    });
    server.route("/dash/a/1.m4s", move |_| {
        Reply::bytes("audio/mp4", seg_a1.to_vec())
    });
    server.route("/dash/a/2.m4s", move |_| {
        Reply::bytes("audio/mp4", seg_a2.to_vec())
    });

    let timeline_mpd_url = server.route("/dash/timeline.mpd", |_| {
        let mpd = r#"<?xml version="1.0" encoding="utf-8"?>
<MPD type="static" mediaPresentationDuration="PT4.0S">
  <Period>
    <AdaptationSet mimeType="video/mp4" contentType="video">
      <Representation id="v1080" bandwidth="4000000" width="1920" height="1080" codecs="avc1.640028">
        <SegmentTemplate initialization="/dash/v1080/init.mp4" media="/dash/v1080/$Number$.m4s" startNumber="1" timescale="1000">
          <SegmentTimeline><S t="0" d="2000" r="1"/></SegmentTimeline>
        </SegmentTemplate>
      </Representation>
      <Representation id="v720" bandwidth="2000000" width="1280" height="720" codecs="avc1.4d401f">
        <SegmentTemplate initialization="/dash/v720/init.mp4" media="/dash/v720/$Number$.m4s" startNumber="1" timescale="1000">
          <SegmentTimeline><S t="0" d="2000" r="1"/></SegmentTimeline>
        </SegmentTemplate>
      </Representation>
    </AdaptationSet>
    <AdaptationSet mimeType="audio/mp4" contentType="audio" lang="en">
      <Representation id="a1" bandwidth="128000" codecs="mp4a.40.2">
        <SegmentTemplate initialization="/dash/a/init.mp4" media="/dash/a/$Number$.m4s" startNumber="1" timescale="1000">
          <SegmentTimeline><S t="0" d="2000" r="1"/></SegmentTimeline>
        </SegmentTemplate>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;
        Reply::bytes("application/dash+xml", mpd.as_bytes().to_vec())
    });

    let list_mpd_url = server.route("/dash/list.mpd", |_| {
        let mpd = r#"<?xml version="1.0" encoding="utf-8"?>
<MPD type="static" mediaPresentationDuration="PT4.0S">
  <Period>
    <AdaptationSet mimeType="video/mp4" contentType="video">
      <Representation id="v720" bandwidth="2000000" width="1280" height="720">
        <SegmentList>
          <Initialization sourceURL="/dash/v720/init.mp4"/>
          <SegmentURL media="/dash/v720/1.m4s"/>
          <SegmentURL media="/dash/v720/2.m4s"/>
        </SegmentList>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;
        Reply::bytes("application/dash+xml", mpd.as_bytes().to_vec())
    });

    let http = HttpDownloader::new()?;
    let engine = MediaEngine::new(http, temp.path(), SidecarEndpoints::default())?;
    let cancel = CancellationToken::new();
    let headers = HeaderMap::new();
    let bw = BandwidthContext::default();

    // 1) Without ffmpeg: downloads video-only cleanly using SegmentTemplate+Timeline.
    let timeline_url = Url::parse(&timeline_mpd_url)?;
    let no_ffmpeg = SidecarSettings::default();
    let inspected_no_ff = engine
        .inspect(&timeline_url, &headers, &no_ffmpeg, &cancel)
        .await?;
    assert_eq!(inspected_no_ff.kind, MediaKind::Dash);
    assert!(!inspected_no_ff.ffmpeg_available);
    assert!(
        inspected_no_ff
            .formats
            .iter()
            .any(|f| f.id == "v720" && f.height == Some(720))
    );

    let out_no_ff = engine
        .download(MediaTransferJob {
            url: &timeline_url,
            request: &MediaRequest {
                kind: MediaKind::Dash,
                format_selector: Some("720p".to_owned()),
                ..MediaRequest::default()
            },
            session_headers: &headers,
            working_dir: &temp.path().join("dash-work-1"),
            ready_dir: &temp.path().join("dash-ready-1"),
            filename_hint: Some("Dash Video"),
            concurrency: 2,
            bandwidth: &bw,
            settings: &no_ffmpeg,
            cancellation: &cancel,
            progress: None,
            on_phase: &|_| {},
        })
        .await?;
    assert_eq!(fs::read(&out_no_ff)?, b"INIT_V|VSEG1|VSEG2|");

    // 2) With mock ffmpeg: downloads video + audio, transitions to PostProcessing, muxes via -c copy.
    let mock_ff = create_mock_ffmpeg(temp.path())?;
    let with_ffmpeg = SidecarSettings {
        ffmpeg_consent: true,
        ffmpeg_custom_path: Some(mock_ff),
        ..SidecarSettings::default()
    };
    let inspected_ff = engine
        .inspect(&timeline_url, &headers, &with_ffmpeg, &cancel)
        .await?;
    assert!(inspected_ff.ffmpeg_available);
    assert!(inspected_ff.formats.iter().any(|f| f.needs_merge));

    let post_proc_seen = Arc::new(AtomicBool::new(false));
    let post_flag = post_proc_seen.clone();
    let out_muxed = engine
        .download(MediaTransferJob {
            url: &timeline_url,
            request: &MediaRequest {
                kind: MediaKind::Dash,
                format_selector: Some("720p".to_owned()),
                ..MediaRequest::default()
            },
            session_headers: &headers,
            working_dir: &temp.path().join("dash-work-2"),
            ready_dir: &temp.path().join("dash-ready-2"),
            filename_hint: Some("Dash Muxed"),
            concurrency: 2,
            bandwidth: &bw,
            settings: &with_ffmpeg,
            cancellation: &cancel,
            progress: None,
            on_phase: &move |phase| {
                if phase == MediaPhase::PostProcessing {
                    post_flag.store(true, Ordering::Release);
                }
            },
        })
        .await?;

    assert!(post_proc_seen.load(Ordering::Acquire));
    let muxed_bytes = fs::read(&out_muxed)?;
    let expected_tail = b"INIT_V|VSEG1|VSEG2|INIT_A|ASEG1|ASEG2";
    assert!(muxed_bytes.ends_with(expected_tail));
    let ff_args = fs::read_to_string(temp.path().join("ffmpeg-args.log"))?;
    assert!(ff_args.contains("-c\ncopy\n") || ff_args.contains("-c copy"));

    // 3) SegmentList support.
    let list_url = Url::parse(&list_mpd_url)?;
    let out_list = engine
        .download(MediaTransferJob {
            url: &list_url,
            request: &MediaRequest {
                kind: MediaKind::Dash,
                format_selector: Some("best".to_owned()),
                ..MediaRequest::default()
            },
            session_headers: &headers,
            working_dir: &temp.path().join("dash-list-work"),
            ready_dir: &temp.path().join("dash-list-ready"),
            filename_hint: Some("Dash List"),
            concurrency: 2,
            bandwidth: &bw,
            settings: &no_ffmpeg,
            cancellation: &cancel,
            progress: None,
            on_phase: &|_| {},
        })
        .await?;
    assert_eq!(fs::read(&out_list)?, b"INIT_V|VSEG1|VSEG2|");
    Ok(())
}

#[tokio::test]
async fn sidecar_manager_consent_sha256_verification_custom_path_and_auto_update() -> TestResult {
    let server = TestServer::start().await?;
    let temp = tempfile::tempdir()?;

    let ytdlp_v1 = b"#!/bin/sh\necho 2026.01.01\n";
    let ytdlp_v2 = b"#!/bin/sh\necho 2026.02.01\n";
    let current_tag = Arc::new(Mutex::new("2026.01.01".to_owned()));
    let return_bad_sha = Arc::new(AtomicBool::new(true));

    let yt_v1_url = server.route("/dl/yt-dlp-v1", move |_| {
        Reply::bytes("application/octet-stream", ytdlp_v1.to_vec())
    });
    let yt_v2_url = server.route("/dl/yt-dlp-v2", move |_| {
        Reply::bytes("application/octet-stream", ytdlp_v2.to_vec())
    });

    let bad_sha_flag = return_bad_sha.clone();
    let tag_for_sums = current_tag.clone();
    let sha_v1 = sha256_hex(ytdlp_v1);
    let sha_v2 = sha256_hex(ytdlp_v2);
    let ytdlp_sums_url = server.route("/dl/SHA2-256SUMS", move |_| {
        if bad_sha_flag.load(Ordering::Acquire) {
            let zero = "0".repeat(64);
            return Reply::bytes(
                "text/plain",
                format!("{zero}  yt-dlp\n{zero}  yt-dlp_linux\n{zero}  yt-dlp.exe\n{zero}  yt-dlp_macos\n")
                    .into_bytes(),
            );
        }
        let is_v2 = tag_for_sums.lock().is_ok_and(|g| *g == "2026.02.01");
        let digest = if is_v2 { &sha_v2 } else { &sha_v1 };
        Reply::bytes(
            "text/plain",
            format!("{digest}  yt-dlp\n{digest}  yt-dlp_linux\n{digest}  yt-dlp.exe\n{digest}  yt-dlp_macos\n")
                .into_bytes(),
        )
    });

    let tag_for_release = current_tag.clone();
    let ytdlp_rel_url = server.route("/releases/yt-dlp/latest", move |_| {
        let tag = tag_for_release
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|_| "2026.01.01".to_owned());
        let dl_url = if tag == "2026.02.01" {
            &yt_v2_url
        } else {
            &yt_v1_url
        };
        let body = serde_json::json!({
            "tag_name": tag,
            "assets": [
                { "name": "yt-dlp_linux", "browser_download_url": dl_url },
                { "name": "yt-dlp_macos", "browser_download_url": dl_url },
                { "name": "yt-dlp.exe", "browser_download_url": dl_url },
                { "name": "SHA2-256SUMS", "browser_download_url": ytdlp_sums_url }
            ]
        });
        Reply::bytes("application/json", body.to_string().into_bytes())
    });

    // Archive extraction + SHA-256 verification test for ffmpeg (tar.gz on Unix, zip on Windows).
    let ff_bin_name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    let ff_payload = b"mock-ffmpeg-binary-payload";
    let (ff_asset_name, ff_archive) = if cfg!(windows) {
        (
            "ffmpeg-n7.1-latest-win64-gpl-7.1.zip",
            make_zip(ff_bin_name, ff_payload)?,
        )
    } else if cfg!(target_os = "macos") {
        ("ffmpeg", ff_payload.to_vec())
    } else {
        (
            "ffmpeg-n7.1-linux64-mock.tar.gz",
            make_tar_gz(ff_bin_name, ff_payload)?,
        )
    };
    let ff_digest = sha256_hex(&ff_archive);
    let ff_dl_url = server.route("/dl/ffmpeg-archive", move |_| {
        Reply::bytes("application/octet-stream", ff_archive.clone())
    });
    let ff_sha_url = server.route("/dl/ffmpeg-sha256", move |_| {
        Reply::bytes("text/plain", ff_digest.as_bytes().to_vec())
    });
    let ff_rel_url = server.route("/releases/ffmpeg/latest", move |_| {
        let body = serde_json::json!({
            "tag_name": "n7.1-mock",
            "assets": [
                { "name": ff_asset_name, "browser_download_url": ff_dl_url },
                { "name": format!("{ff_asset_name}.sha256"), "browser_download_url": ff_sha_url }
            ]
        });
        Reply::bytes("application/json", body.to_string().into_bytes())
    });

    let endpoints = SidecarEndpoints {
        ytdlp_release_url: ytdlp_rel_url,
        ffmpeg_release_url: ff_rel_url,
        auto_update_interval_secs: 0,
    };
    let engine = MediaEngine::new(HttpDownloader::new()?, temp.path(), endpoints)?;
    let cancel = CancellationToken::new();

    // Consent gate must refuse downloads until user explicitly opts in.
    let err = engine
        .install_sidecar(SidecarKind::YtDlp, &SidecarSettings::default(), &cancel)
        .await
        .err()
        .ok_or("expected ConsentRequired error")?;
    assert!(matches!(
        err,
        MediaError::ConsentRequired(SidecarKind::YtDlp)
    ));

    // Mismatched SHA-256 must fail loudly before replacing binary.
    let mut settings = SidecarSettings {
        ytdlp_consent: true,
        ytdlp_auto_update: true,
        ffmpeg_consent: true,
        ..SidecarSettings::default()
    };
    let mismatch = engine
        .install_sidecar(SidecarKind::YtDlp, &settings, &cancel)
        .await
        .err()
        .ok_or("expected ChecksumMismatch error")?;
    assert!(matches!(mismatch, MediaError::ChecksumMismatch { .. }));

    // Verified yt-dlp installation succeeds.
    return_bad_sha.store(false, Ordering::Release);
    let status_yt = engine
        .install_sidecar(SidecarKind::YtDlp, &settings, &cancel)
        .await?;
    assert!(status_yt.installed);
    assert!(status_yt.managed);
    assert_eq!(status_yt.version.as_deref(), Some("2026.01.01"));

    // Auto-update detects 2026.02.01 and updates binary cleanly.
    if let Ok(mut guard) = current_tag.lock() {
        *guard = "2026.02.01".to_owned();
    }
    let updated_ver = engine
        .sidecars()
        .maybe_auto_update_ytdlp(&settings, engine.client(), &cancel)
        .await?;
    assert_eq!(updated_ver.as_deref(), Some("2026.02.01"));

    // Verified ffmpeg extraction succeeds.
    let status_ff = engine
        .install_sidecar(SidecarKind::Ffmpeg, &settings, &cancel)
        .await?;
    assert!(status_ff.installed);
    let ff_installed_path = status_ff.path.ok_or("missing installed ffmpeg path")?;
    assert_eq!(fs::read(ff_installed_path)?, ff_payload);

    // Custom binary override takes precedence over managed path.
    let custom_ff = create_mock_ffmpeg(temp.path())?;
    settings.ffmpeg_custom_path = Some(custom_ff.clone());
    let combined = engine.sidecar_status(&settings).await;
    assert!(combined.ffmpeg.installed);
    assert!(!combined.ffmpeg.managed);
    assert_eq!(combined.ffmpeg.path, Some(custom_ff));
    assert!(combined.ffmpeg.version.is_some());
    Ok(())
}

#[tokio::test]
async fn ytdlp_json_fixtures_cookie_forwarding_and_hybrid_download() -> TestResult {
    // 1) Pure fixture inspection (single video + playlist).
    let video_fixture = serde_json::json!({
        "title": "Sample Conference Talk",
        "ext": "mp4",
        "formats": [
            {
                "format_id": "22",
                "ext": "mp4",
                "height": 720,
                "width": 1280,
                "vcodec": "avc1",
                "acodec": "mp4a",
                "url": "https://cdn.invalid/720p.mp4"
            },
            {
                "format_id": "137",
                "ext": "mp4",
                "height": 1080,
                "width": 1920,
                "vcodec": "avc1.640028",
                "acodec": "none",
                "url": "https://cdn.invalid/1080v.mp4"
            },
            {
                "format_id": "140",
                "ext": "m4a",
                "vcodec": "none",
                "acodec": "mp4a.40.2",
                "url": "https://cdn.invalid/audio.m4a"
            }
        ]
    });
    let page_url = Url::parse("https://video.example/watch?v=abc")?;
    let single_no_ff = parse_ytdlp_json(&page_url, video_fixture.to_string().as_bytes(), false)?;
    assert_eq!(single_no_ff.title, "Sample Conference Talk");
    assert!(single_no_ff.formats.iter().any(|f| f.id == "720p"));
    assert!(!single_no_ff.formats.iter().any(|f| f.id == "1080p"));

    let single_with_ff = parse_ytdlp_json(&page_url, video_fixture.to_string().as_bytes(), true)?;
    assert!(single_with_ff.formats.iter().any(|f| f.id == "1080p"));

    let playlist_fixture = serde_json::json!({
        "_type": "playlist",
        "title": "Rust Lectures",
        "entries": [
            { "id": "ep1", "title": "Ownership", "url": "https://video.example/watch?v=1", "duration": 305.0 },
            { "id": "ep2", "title": "Async Rust", "webpage_url": "https://video.example/watch?v=2", "duration": 640.0 }
        ]
    });
    let playlist_inspected = parse_ytdlp_json(
        &Url::parse("https://video.example/playlist?list=1")?,
        playlist_fixture.to_string().as_bytes(),
        true,
    )?;
    assert_eq!(
        playlist_inspected.playlist_title.as_deref(),
        Some("Rust Lectures")
    );
    assert_eq!(playlist_inspected.playlist_items.len(), 2);
    assert_eq!(
        playlist_inspected.playlist_items[1].filename,
        "Async Rust.mp4"
    );

    // 2) Hybrid native HTTP handoff vs subprocess fallback + Netscape cookies.txt forwarding.
    let server = TestServer::start().await?;
    let temp = tempfile::tempdir()?;
    let direct_file = server.file(FileSpec::random(42, 2048).filename("native-talk.mp4"));
    let expected_direct = direct_file.payload().bytes(0..2048);

    let json_path = temp.path().join("ytdlp-info.json");
    let direct_json = serde_json::json!({
        "title": "Native Direct Video",
        "ext": "mp4",
        "formats": [{
            "format_id": "18",
            "ext": "mp4",
            "height": 720,
            "width": 1280,
            "protocol": "https",
            "vcodec": "avc1",
            "acodec": "mp4a",
            "url": direct_file.url(),
            "http_headers": { "X-Custom-Media": "yes" }
        }]
    });
    fs::write(&json_path, direct_json.to_string())?;

    let cookie_capture = temp.path().join("captured-cookies.txt");
    let sub_payload = b"ID3-MOCK-MP3-PAYLOAD";
    let mock_ytdlp = create_mock_ytdlp(temp.path(), &json_path, sub_payload, &cookie_capture)?;

    let engine = MediaEngine::new(
        HttpDownloader::new()?,
        temp.path(),
        SidecarEndpoints::default(),
    )?;
    let settings = SidecarSettings {
        ytdlp_consent: true,
        ytdlp_custom_path: Some(mock_ytdlp),
        ..SidecarSettings::default()
    };
    let cancel = CancellationToken::new();
    let bw = BandwidthContext::default();

    let mut headers = HeaderMap::new();
    headers.insert(COOKIE, HeaderValue::from_static("auth=tok_999; pref=hd"));
    let watch_url = Url::parse(&server.route("/watch", |_| Reply::html("ok")))?;

    // Inspect via yt-dlp -J subprocess.
    let inspected = engine
        .inspect(&watch_url, &headers, &settings, &cancel)
        .await?;
    assert_eq!(inspected.kind, MediaKind::YtDlp);
    assert_eq!(inspected.title, "Native Direct Video");

    // Verify cookie file captured during inspect contains auth cookie & was 0o600 on Unix.
    let captured = fs::read_to_string(&cookie_capture)?;
    assert!(captured.contains("# Netscape HTTP Cookie File"));
    assert!(captured.contains("auth\ttok_999"));
    #[cfg(unix)]
    assert!(captured.contains("PERMS=600"));

    // Progressive download hands off to native ruscello-http engine (faster segmented/direct HTTP).
    let out_native = engine
        .download(MediaTransferJob {
            url: &watch_url,
            request: &MediaRequest {
                kind: MediaKind::YtDlp,
                format_selector: Some("best".to_owned()),
                ..MediaRequest::default()
            },
            session_headers: &headers,
            working_dir: &temp.path().join("yt-work-native"),
            ready_dir: &temp.path().join("yt-ready-native"),
            filename_hint: None,
            concurrency: 2,
            bandwidth: &bw,
            settings: &settings,
            cancellation: &cancel,
            progress: None,
            on_phase: &|_| {},
        })
        .await?;
    assert_eq!(fs::read(&out_native)?, expected_direct);
    assert!(!direct_file.requests().is_empty());

    // Audio-only MP3 preset invokes yt-dlp subprocess and parses progress lines.
    let (prog_tx, prog_rx) = watch::channel(DownloadProgress::default());
    let out_sub = engine
        .download(MediaTransferJob {
            url: &watch_url,
            request: &MediaRequest {
                kind: MediaKind::YtDlp,
                format_selector: Some("audio-mp3".to_owned()),
                ..MediaRequest::default()
            },
            session_headers: &headers,
            working_dir: &temp.path().join("yt-work-sub"),
            ready_dir: &temp.path().join("yt-ready-sub"),
            filename_hint: None,
            concurrency: 2,
            bandwidth: &bw,
            settings: &settings,
            cancellation: &cancel,
            progress: Some(&prog_tx),
            on_phase: &|_| {},
        })
        .await?;
    assert_eq!(fs::read(&out_sub)?, sub_payload);
    assert_eq!(prog_rx.borrow().completed, 100);
    Ok(())
}
