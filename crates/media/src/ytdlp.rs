use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
};

use reqwest::Url;
use ruscello_http::{
    DownloadOptions, HttpDownloader, RetryPolicy, TransferLink, sanitize_filename,
};
use serde::Deserialize;
use tokio::{
    io::{AsyncBufReadExt as _, BufReader},
    process::Command,
    sync::watch,
};
use tokio_util::sync::CancellationToken;

use crate::{
    FormatPreset, MediaError, MediaFormat, MediaInspection, MediaKind, MediaPhase, PlaylistItem,
    ffmpeg, hls,
    net::{
        BandwidthContext, ensure_filename, parse_http_url, report_progress, split_headers,
        write_netscape_cookies_file,
    },
};

#[derive(Debug, Deserialize)]
struct RawInfoJson {
    #[serde(rename = "_type")]
    kind: Option<String>,
    title: Option<String>,
    ext: Option<String>,
    url: Option<String>,
    protocol: Option<String>,
    vcodec: Option<String>,
    acodec: Option<String>,
    filesize: Option<u64>,
    filesize_approx: Option<u64>,
    #[serde(default)]
    http_headers: HashMap<String, String>,
    #[serde(default)]
    formats: Vec<RawFormatJson>,
    #[serde(default)]
    entries: Vec<RawEntryJson>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawFormatJson {
    format_id: String,
    ext: Option<String>,
    url: Option<String>,
    protocol: Option<String>,
    height: Option<u32>,
    width: Option<u32>,
    tbr: Option<f64>,
    filesize: Option<u64>,
    filesize_approx: Option<u64>,
    vcodec: Option<String>,
    acodec: Option<String>,
    #[serde(default)]
    http_headers: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct RawEntryJson {
    id: Option<String>,
    title: Option<String>,
    url: Option<String>,
    webpage_url: Option<String>,
    duration: Option<f64>,
}

#[derive(Debug, Clone)]
enum TransferPlan {
    ProgressiveHttp {
        url: Url,
        headers: Vec<(String, String)>,
        ext: String,
    },
    NativeHls {
        url: Url,
        headers: Vec<(String, String)>,
    },
    SplitHttp {
        video_url: Url,
        audio_url: Url,
        headers: Vec<(String, String)>,
        ext: String,
    },
    Subprocess {
        format_expr: String,
        extract_mp3: bool,
    },
}

/// Runs `yt-dlp -J` to extract metadata, selectable quality/format presets, and playlists.
pub(crate) async fn inspect(
    ytdlp_bin: &Path,
    url: &Url,
    cookie_header: Option<&str>,
    user_agent: Option<&str>,
    ffmpeg_available: bool,
    cancellation: &CancellationToken,
) -> Result<MediaInspection, MediaError> {
    let raw = run_json_dump(
        ytdlp_bin,
        url,
        cookie_header,
        user_agent,
        true,
        cancellation,
    )
    .await?;
    Ok(parse_info_json(url, raw, ffmpeg_available))
}

/// Parses a recorded `yt-dlp -J` JSON payload into [MediaInspection] (also used cleanly in tests).
pub fn parse_ytdlp_json(
    source_url: &Url,
    json_bytes: &[u8],
    ffmpeg_available: bool,
) -> Result<MediaInspection, MediaError> {
    let raw: RawInfoJson = serde_json::from_slice(json_bytes).map_err(|error| {
        MediaError::ManifestParse(format!("invalid yt-dlp JSON metadata: {error}"))
    })?;
    Ok(parse_info_json(source_url, raw, ffmpeg_available))
}

/// Executes a `yt-dlp`-backed media download: uses native Rust engine when possible, else subprocess.
pub(crate) struct YtDlpJob<'a> {
    pub ytdlp_bin: &'a Path,
    pub ffmpeg_bin: Option<&'a Path>,
    pub http: &'a HttpDownloader,
    pub client: &'a reqwest::Client,
    pub url: &'a Url,
    pub cookie_header: Option<&'a str>,
    pub user_agent: Option<&'a str>,
    pub working_dir: &'a Path,
    pub ready_dir: &'a Path,
    pub filename_hint: Option<&'a str>,
    pub format_selector: Option<&'a str>,
    pub concurrency: usize,
    pub bandwidth: &'a BandwidthContext,
    pub cancellation: &'a CancellationToken,
    pub progress: Option<&'a watch::Sender<ruscello_http::DownloadProgress>>,
    pub on_phase: &'a (dyn Fn(MediaPhase) + Send + Sync),
}

pub(crate) async fn download(job: YtDlpJob<'_>) -> Result<PathBuf, MediaError> {
    let raw = run_json_dump(
        job.ytdlp_bin,
        job.url,
        job.cookie_header,
        job.user_agent,
        false,
        job.cancellation,
    )
    .await?;

    let title = job
        .filename_hint
        .map(str::to_owned)
        .or_else(|| raw.title.clone())
        .unwrap_or_else(|| crate::net::title_from_url(job.url));
    let preset = job
        .format_selector
        .map(FormatPreset::parse)
        .unwrap_or_default();

    let plan = select_transfer_plan(&raw, &preset, job.ffmpeg_bin.is_some());
    fs::create_dir_all(job.ready_dir)?;

    match plan {
        TransferPlan::ProgressiveHttp { url, headers, ext } => {
            let final_name = ensure_filename(&title, &ext);
            let output_path = job.ready_dir.join(&final_name);
            let temp_dir = job.working_dir.join("ytdlp-http");
            fs::create_dir_all(&temp_dir)?;
            download_native_http(NativeHttpParams {
                http: job.http,
                url: &url,
                headers: &headers,
                cookie_header: job.cookie_header,
                filename: &final_name,
                working_dir: &temp_dir,
                completion_dir: job.ready_dir,
                bandwidth: job.bandwidth,
                cancellation: job.cancellation,
                progress: job.progress,
            })
            .await?;
            Ok(find_single_ready_file(job.ready_dir)?.unwrap_or(output_path))
        }
        TransferPlan::NativeHls { url, headers } => {
            let header_map = crate::net::build_headers(&headers, job.cookie_header);
            hls::download(hls::NativeStreamJob {
                client: job.client,
                url: &url,
                headers: &header_map,
                working_dir: job.working_dir,
                ready_dir: job.ready_dir,
                filename_hint: Some(&title),
                format_selector: job.format_selector,
                variant_uri: None,
                dash_video_id: None,
                dash_audio_id: None,
                concurrency: job.concurrency,
                bandwidth: job.bandwidth,
                ffmpeg_bin: job.ffmpeg_bin,
                cancellation: job.cancellation,
                progress: job.progress,
                on_phase: job.on_phase,
            })
            .await
        }
        TransferPlan::SplitHttp {
            video_url,
            audio_url,
            headers,
            ext,
        } => {
            if let Some(ffmpeg_bin) = job.ffmpeg_bin {
                let stage_v = job.working_dir.join("ytdlp-split-video");
                let stage_a = job.working_dir.join("ytdlp-split-audio");
                let ready_v = job.working_dir.join("ytdlp-split-v-ready");
                let ready_a = job.working_dir.join("ytdlp-split-a-ready");
                fs::create_dir_all(&stage_v)?;
                fs::create_dir_all(&stage_a)?;
                fs::create_dir_all(&ready_v)?;
                fs::create_dir_all(&ready_a)?;

                let v_file = download_native_http(NativeHttpParams {
                    http: job.http,
                    url: &video_url,
                    headers: &headers,
                    cookie_header: job.cookie_header,
                    filename: "video.bin",
                    working_dir: &stage_v,
                    completion_dir: &ready_v,
                    bandwidth: job.bandwidth,
                    cancellation: job.cancellation,
                    progress: job.progress,
                })
                .await?;
                let a_file = download_native_http(NativeHttpParams {
                    http: job.http,
                    url: &audio_url,
                    headers: &headers,
                    cookie_header: job.cookie_header,
                    filename: "audio.bin",
                    working_dir: &stage_a,
                    completion_dir: &ready_a,
                    bandwidth: job.bandwidth,
                    cancellation: job.cancellation,
                    progress: None,
                })
                .await?;

                (job.on_phase)(MediaPhase::PostProcessing);
                let out_path = job.ready_dir.join(ensure_filename(&title, &ext));
                ffmpeg::remux_av(ffmpeg_bin, &v_file, &a_file, &out_path, job.cancellation).await?;
                let _ = fs::remove_dir_all(&stage_v);
                let _ = fs::remove_dir_all(&stage_a);
                let _ = fs::remove_dir_all(&ready_v);
                let _ = fs::remove_dir_all(&ready_a);
                Ok(out_path)
            } else {
                download_subprocess(&job, &title, "best[vcodec!=none][acodec!=none]/best", false)
                    .await
            }
        }
        TransferPlan::Subprocess {
            format_expr,
            extract_mp3,
        } => download_subprocess(&job, &title, &format_expr, extract_mp3).await,
    }
}

struct NativeHttpParams<'a> {
    http: &'a HttpDownloader,
    url: &'a Url,
    headers: &'a [(String, String)],
    cookie_header: Option<&'a str>,
    filename: &'a str,
    working_dir: &'a Path,
    completion_dir: &'a Path,
    bandwidth: &'a BandwidthContext,
    cancellation: &'a CancellationToken,
    progress: Option<&'a watch::Sender<ruscello_http::DownloadProgress>>,
}

async fn download_native_http(params: NativeHttpParams<'_>) -> Result<PathBuf, MediaError> {
    let header_map = crate::net::build_headers(params.headers, params.cookie_header);
    let options = DownloadOptions {
        directory: params.working_dir.to_path_buf(),
        cancellation: params.cancellation.clone(),
        retry: RetryPolicy::default(),
        bandwidth: params.bandwidth.category.clone(),
        download_bandwidth: None,
        progress: params.progress.cloned(),
        resolved: Some(TransferLink {
            url: params.url.clone(),
            headers: header_map,
            expires: None,
            max_connections: None,
        }),
        filename_hint: Some(sanitize_filename(params.filename)),
        completion_directory: Some(params.completion_dir.to_path_buf()),
        ..DownloadOptions::default()
    };
    params
        .http
        .download(params.url, options)
        .await
        .map_err(|error| MediaError::SidecarProcess {
            kind: crate::SidecarKind::YtDlp,
            detail: error.to_string(),
        })
}

async fn download_subprocess(
    job: &YtDlpJob<'_>,
    title: &str,
    format_expr: &str,
    extract_mp3: bool,
) -> Result<PathBuf, MediaError> {
    let _cookie_guard = job
        .cookie_header
        .map(|cookie| write_netscape_cookies_file(job.url, cookie))
        .transpose()?;

    let out_dir = job.working_dir.join("ytdlp");
    fs::create_dir_all(&out_dir)?;
    let safe_stem = sanitize_filename(title);
    let output_template = out_dir.join(format!("{safe_stem}.%(ext)s"));

    let mut command = Command::new(job.ytdlp_bin);
    command
        .arg("--newline")
        .arg("--no-warnings")
        .arg("--no-mtime")
        .arg("--progress-template")
        .arg("download:RUSCELLO|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s")
        .arg("--print")
        .arg("after_move:filepath")
        .arg("-f")
        .arg(format_expr)
        .arg("-o")
        .arg(&output_template);

    if extract_mp3 {
        command.arg("-x").arg("--audio-format").arg("mp3");
    }
    if let Some(ffmpeg_bin) = job.ffmpeg_bin {
        command.arg("--ffmpeg-location").arg(ffmpeg_bin);
    }
    if let Some(ref file) = _cookie_guard {
        command.arg("--cookies").arg(file.path());
    }
    if let Some(ua) = job.user_agent {
        command.arg("--user-agent").arg(ua);
    }
    command
        .arg(job.url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command
        .spawn()
        .map_err(|error| MediaError::SidecarProcess {
            kind: crate::SidecarKind::YtDlp,
            detail: format!("failed to start yt-dlp: {error}"),
        })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| MediaError::SidecarProcess {
            kind: crate::SidecarKind::YtDlp,
            detail: "missing stdout pipe".to_owned(),
        })?;
    let stderr = child.stderr.take();

    let stderr_task = tokio::spawn(async move {
        let mut collected = String::new();
        if let Some(err) = stderr {
            let mut reader = BufReader::new(err).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if !collected.is_empty() {
                    collected.push('\n');
                }
                collected.push_str(&line);
            }
        }
        collected
    });

    let mut lines = BufReader::new(stdout).lines();
    let mut final_file: Option<PathBuf> = None;

    loop {
        tokio::select! {
            () = job.cancellation.cancelled() => {
                let _ = child.kill().await;
                return Err(MediaError::Cancelled);
            }
            next = lines.next_line() => {
                match next {
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if let Some((done, total)) = parse_progress_line(trimmed) {
                            report_progress(job.progress, done, total);
                        } else if !trimmed.is_empty() && !trimmed.starts_with('[') {
                            let candidate = PathBuf::from(trimmed);
                            if candidate.exists() {
                                final_file = Some(candidate);
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        return Err(MediaError::SidecarProcess {
                            kind: crate::SidecarKind::YtDlp,
                            detail: format!("failed reading yt-dlp stdout: {error}"),
                        });
                    }
                }
            }
        }
    }

    let status = tokio::select! {
        () = job.cancellation.cancelled() => {
            let _ = child.kill().await;
            return Err(MediaError::Cancelled);
        }
        res = child.wait() => res?,
    };
    let stderr_text = stderr_task.await.unwrap_or_default();

    if !status.success() {
        return Err(MediaError::SidecarProcess {
            kind: crate::SidecarKind::YtDlp,
            detail: stderr_text.trim().to_owned(),
        });
    }

    let produced = final_file
        .or_else(|| find_latest_regular_file(&out_dir))
        .ok_or_else(|| MediaError::SidecarProcess {
            kind: crate::SidecarKind::YtDlp,
            detail: "yt-dlp finished without producing an output file".to_owned(),
        })?;

    let target_name = produced
        .file_name()
        .and_then(|v| v.to_str())
        .map(sanitize_filename)
        .unwrap_or_else(|| ensure_filename(&safe_stem, "mp4"));
    let ready_target = job.ready_dir.join(target_name);
    move_file_atomic(&produced, &ready_target)?;
    Ok(ready_target)
}

async fn run_json_dump(
    ytdlp_bin: &Path,
    url: &Url,
    cookie_header: Option<&str>,
    user_agent: Option<&str>,
    flat_playlist: bool,
    cancellation: &CancellationToken,
) -> Result<RawInfoJson, MediaError> {
    let _cookie_guard = cookie_header
        .map(|cookie| write_netscape_cookies_file(url, cookie))
        .transpose()?;

    let mut command = Command::new(ytdlp_bin);
    command.arg("-J").arg("--no-warnings");
    if flat_playlist {
        command.arg("--flat-playlist");
    }
    if let Some(ref file) = _cookie_guard {
        command.arg("--cookies").arg(file.path());
    }
    if let Some(ua) = user_agent {
        command.arg("--user-agent").arg(ua);
    }
    command
        .arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = command
        .spawn()
        .map_err(|error| MediaError::SidecarProcess {
            kind: crate::SidecarKind::YtDlp,
            detail: format!("failed to launch yt-dlp: {error}"),
        })?;

    let output = tokio::select! {
        () = cancellation.cancelled() => return Err(MediaError::Cancelled),
        done = child.wait_with_output() => done?,
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(MediaError::SidecarProcess {
            kind: crate::SidecarKind::YtDlp,
            detail: stderr.trim().to_owned(),
        });
    }

    serde_json::from_slice(&output.stdout).map_err(|error| {
        MediaError::ManifestParse(format!("failed to parse yt-dlp -J output: {error}"))
    })
}

fn parse_info_json(source_url: &Url, raw: RawInfoJson, ffmpeg_available: bool) -> MediaInspection {
    let is_playlist = raw.kind.as_deref() == Some("playlist") || !raw.entries.is_empty();
    let title = raw
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| crate::net::title_from_url(source_url));

    if is_playlist {
        let items: Vec<PlaylistItem> = raw
            .entries
            .into_iter()
            .enumerate()
            .filter_map(|(idx, entry)| {
                let item_url = entry
                    .webpage_url
                    .or(entry.url)
                    .and_then(|u| resolve_entry_url(source_url, &u))?;
                let id = entry.id.unwrap_or_else(|| format!("{}", idx + 1));
                let item_title = entry
                    .title
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or_else(|| id.clone());
                let filename = ensure_filename(&item_title, "mp4");
                Some(PlaylistItem {
                    index: idx + 1,
                    id,
                    title: item_title,
                    url: item_url,
                    duration_secs: entry.duration.map(|d| d.max(0.0) as u64),
                    filename,
                })
            })
            .collect();

        return MediaInspection {
            source_url: source_url.as_str().to_owned(),
            kind: MediaKind::YtDlp,
            title: title.clone(),
            suggested_filename: sanitize_filename(&title),
            ffmpeg_available,
            formats: standard_preset_formats(ffmpeg_available),
            playlist_title: Some(title),
            playlist_items: items,
        };
    }

    let mut formats = Vec::new();
    let best_ext = raw.ext.as_deref().unwrap_or("mp4").to_owned();
    formats.push(MediaFormat {
        id: "best".to_owned(),
        label: if ffmpeg_available {
            "Best (Auto)".to_owned()
        } else {
            "Best (Progressive)".to_owned()
        },
        preset: FormatPreset::Best,
        ext: best_ext.clone(),
        height: raw.formats.iter().filter_map(|f| f.height).max(),
        width: raw.formats.iter().filter_map(|f| f.width).max(),
        bandwidth_bps: None,
        filesize: raw.filesize.or(raw.filesize_approx),
        vcodec: raw.vcodec.clone(),
        acodec: raw.acodec.clone(),
        has_video: true,
        has_audio: true,
        needs_merge: false,
        url: raw.url.clone(),
        audio_url: None,
        audio_format_id: None,
        headers: map_headers(&raw.http_headers),
    });

    // Offer clean standard heights present in the stream (2160p/1080p/720p/480p).
    for target_h in [2160_u32, 1080, 720, 480] {
        let has_exact_or_higher = raw.formats.iter().any(|f| {
            f.height.is_some_and(|h| h == target_h) && (ffmpeg_available || is_progressive(f))
        });
        if has_exact_or_higher {
            let matching = find_best_video_format(&raw.formats, Some(target_h), ffmpeg_available);
            formats.push(MediaFormat {
                id: format!("{target_h}p"),
                label: format!("{target_h}p"),
                preset: FormatPreset::MaxHeight(target_h),
                ext: matching
                    .and_then(|f| f.ext.clone())
                    .unwrap_or_else(|| "mp4".to_owned()),
                height: Some(target_h),
                width: matching.and_then(|f| f.width),
                bandwidth_bps: matching.and_then(|f| f.tbr.map(|kbps| (kbps * 1000.0) as u64)),
                filesize: matching.and_then(|f| f.filesize.or(f.filesize_approx)),
                vcodec: matching.and_then(|f| f.vcodec.clone()),
                acodec: matching.and_then(|f| f.acodec.clone()),
                has_video: true,
                has_audio: true,
                needs_merge: matching.is_some_and(|f| ffmpeg_available && !is_progressive(f)),
                url: matching.and_then(|f| f.url.clone()),
                audio_url: None,
                audio_format_id: None,
                headers: matching
                    .map(|f| map_headers(&f.http_headers))
                    .unwrap_or_default(),
            });
        }
    }

    if ffmpeg_available {
        formats.push(MediaFormat {
            id: "audio-mp3".to_owned(),
            label: "Audio Only (MP3)".to_owned(),
            preset: FormatPreset::AudioOnly { mp3: true },
            ext: "mp3".to_owned(),
            height: None,
            width: None,
            bandwidth_bps: None,
            filesize: None,
            vcodec: None,
            acodec: Some("mp3".to_owned()),
            has_video: false,
            has_audio: true,
            needs_merge: false,
            url: None,
            audio_url: None,
            audio_format_id: None,
            headers: Vec::new(),
        });
    }

    if let Some(audio) = find_best_audio_format(&raw.formats) {
        formats.push(MediaFormat {
            id: "audio".to_owned(),
            label: format!(
                "Audio Only ({})",
                audio.ext.as_deref().unwrap_or("m4a").to_ascii_uppercase()
            ),
            preset: FormatPreset::AudioOnly { mp3: false },
            ext: audio.ext.as_deref().unwrap_or("m4a").to_owned(),
            height: None,
            width: None,
            bandwidth_bps: audio.tbr.map(|kbps| (kbps * 1000.0) as u64),
            filesize: audio.filesize.or(audio.filesize_approx),
            vcodec: None,
            acodec: audio.acodec.clone(),
            has_video: false,
            has_audio: true,
            needs_merge: false,
            url: audio.url.clone(),
            audio_url: None,
            audio_format_id: Some(audio.format_id.clone()),
            headers: map_headers(&audio.http_headers),
        });
    }

    MediaInspection {
        source_url: source_url.as_str().to_owned(),
        kind: MediaKind::YtDlp,
        title: title.clone(),
        suggested_filename: ensure_filename(&title, &best_ext),
        ffmpeg_available,
        formats,
        playlist_title: None,
        playlist_items: Vec::new(),
    }
}

fn standard_preset_formats(ffmpeg_available: bool) -> Vec<MediaFormat> {
    let mut formats = vec![
        preset_format("best", "Best (Auto)", FormatPreset::Best, "mp4", Some(1080)),
        preset_format(
            "1080p",
            "1080p",
            FormatPreset::MaxHeight(1080),
            "mp4",
            Some(1080),
        ),
        preset_format(
            "720p",
            "720p",
            FormatPreset::MaxHeight(720),
            "mp4",
            Some(720),
        ),
        preset_format(
            "480p",
            "480p",
            FormatPreset::MaxHeight(480),
            "mp4",
            Some(480),
        ),
    ];
    if ffmpeg_available {
        formats.push(preset_format(
            "audio-mp3",
            "Audio Only (MP3)",
            FormatPreset::AudioOnly { mp3: true },
            "mp3",
            None,
        ));
    }
    formats.push(preset_format(
        "audio",
        "Audio Only",
        FormatPreset::AudioOnly { mp3: false },
        "m4a",
        None,
    ));
    formats
}

fn preset_format(
    id: &str,
    label: &str,
    preset: FormatPreset,
    ext: &str,
    height: Option<u32>,
) -> MediaFormat {
    MediaFormat {
        id: id.to_owned(),
        label: label.to_owned(),
        preset,
        ext: ext.to_owned(),
        height,
        width: None,
        bandwidth_bps: None,
        filesize: None,
        vcodec: None,
        acodec: None,
        has_video: height.is_some() || id == "best",
        has_audio: true,
        needs_merge: false,
        url: None,
        audio_url: None,
        audio_format_id: None,
        headers: Vec::new(),
    }
}

fn select_transfer_plan(
    raw: &RawInfoJson,
    preset: &FormatPreset,
    ffmpeg_available: bool,
) -> TransferPlan {
    if matches!(preset, FormatPreset::AudioOnly { mp3: true }) {
        return TransferPlan::Subprocess {
            format_expr: "bestaudio/best".to_owned(),
            extract_mp3: true,
        };
    }

    if matches!(preset, FormatPreset::AudioOnly { mp3: false }) {
        if let Some(audio) = find_best_audio_format(&raw.formats)
            && let Some(plan) = plan_single_format(audio)
        {
            return plan;
        }
        return TransferPlan::Subprocess {
            format_expr: "bestaudio/best".to_owned(),
            extract_mp3: false,
        };
    }

    if let FormatPreset::FormatId(fid) = preset {
        if let Some(fmt) = raw.formats.iter().find(|f| f.format_id == *fid) {
            if is_progressive(fmt)
                && let Some(plan) = plan_single_format(fmt)
            {
                return plan;
            }
            if ffmpeg_available
                && !is_progressive(fmt)
                && let Some(audio) = find_best_audio_format(&raw.formats)
                && let Some((v_url, a_url)) = direct_pair(fmt, audio)
            {
                return TransferPlan::SplitHttp {
                    video_url: v_url,
                    audio_url: a_url,
                    headers: map_headers(&fmt.http_headers),
                    ext: fmt.ext.clone().unwrap_or_else(|| "mp4".to_owned()),
                };
            }
        }
        return TransferPlan::Subprocess {
            format_expr: fid.clone(),
            extract_mp3: false,
        };
    }

    let max_h = match preset {
        FormatPreset::MaxHeight(h) => Some(*h),
        _ => None,
    };

    // Prefer native progressive HTTP or HLS when `!ffmpeg_available` or when already muxed.
    if let Some(prog) = find_best_progressive_format(&raw.formats, max_h)
        && (!ffmpeg_available || max_h.is_none_or(|h| prog.height.is_some_and(|ph| ph == h)))
        && let Some(plan) = plan_single_format(prog)
    {
        return plan;
    }

    if !ffmpeg_available {
        if let Some(prog) = find_best_progressive_format(&raw.formats, max_h)
            && let Some(plan) = plan_single_format(prog)
        {
            return plan;
        }
        if let Some(ref url_str) = raw.url
            && is_direct_http_protocol(raw.protocol.as_deref())
            && let Ok(url) = parse_http_url(url_str)
        {
            return TransferPlan::ProgressiveHttp {
                url,
                headers: map_headers(&raw.http_headers),
                ext: raw.ext.clone().unwrap_or_else(|| "mp4".to_owned()),
            };
        }
        let expr = max_h.map_or_else(
            || "best[vcodec!=none][acodec!=none]/best".to_owned(),
            |h| format!("best[height<={h}][vcodec!=none][acodec!=none]/best[height<={h}]/best"),
        );
        return TransferPlan::Subprocess {
            format_expr: expr,
            extract_mp3: false,
        };
    }

    // With ffmpeg available, check split video + audio direct URLs or HLS.
    if let Some(best_v) = find_best_video_format(&raw.formats, max_h, true) {
        if is_progressive(best_v)
            && let Some(plan) = plan_single_format(best_v)
        {
            return plan;
        }
        if let Some(best_a) = find_best_audio_format(&raw.formats)
            && let Some((v_url, a_url)) = direct_pair(best_v, best_a)
        {
            return TransferPlan::SplitHttp {
                video_url: v_url,
                audio_url: a_url,
                headers: map_headers(&best_v.http_headers),
                ext: best_v.ext.clone().unwrap_or_else(|| "mp4".to_owned()),
            };
        }
    }

    if let Some(ref url_str) = raw.url
        && is_direct_http_protocol(raw.protocol.as_deref())
        && let Ok(url) = parse_http_url(url_str)
    {
        return TransferPlan::ProgressiveHttp {
            url,
            headers: map_headers(&raw.http_headers),
            ext: raw.ext.clone().unwrap_or_else(|| "mp4".to_owned()),
        };
    }

    let expr = max_h.map_or_else(
        || "bestvideo+bestaudio/best".to_owned(),
        |h| format!("bestvideo[height<={h}]+bestaudio/best[height<={h}]/best"),
    );
    TransferPlan::Subprocess {
        format_expr: expr,
        extract_mp3: false,
    }
}

fn plan_single_format(fmt: &RawFormatJson) -> Option<TransferPlan> {
    let raw_url = fmt.url.as_deref()?;
    let url = parse_http_url(raw_url).ok()?;
    let proto = fmt.protocol.as_deref().unwrap_or("");
    if proto.starts_with("m3u8") || crate::net::is_hls_url(&url) {
        return Some(TransferPlan::NativeHls {
            url,
            headers: map_headers(&fmt.http_headers),
        });
    }
    if is_direct_http_protocol(fmt.protocol.as_deref()) {
        return Some(TransferPlan::ProgressiveHttp {
            url,
            headers: map_headers(&fmt.http_headers),
            ext: fmt.ext.clone().unwrap_or_else(|| "mp4".to_owned()),
        });
    }
    None
}

fn direct_pair(video: &RawFormatJson, audio: &RawFormatJson) -> Option<(Url, Url)> {
    if !is_direct_http_protocol(video.protocol.as_deref())
        || !is_direct_http_protocol(audio.protocol.as_deref())
    {
        return None;
    }
    let v = parse_http_url(video.url.as_deref()?).ok()?;
    let a = parse_http_url(audio.url.as_deref()?).ok()?;
    Some((v, a))
}

fn is_direct_http_protocol(protocol: Option<&str>) -> bool {
    matches!(protocol, None | Some("http" | "https"))
}

fn is_progressive(fmt: &RawFormatJson) -> bool {
    fmt.vcodec.as_deref().is_some_and(|v| v != "none")
        && fmt.acodec.as_deref().is_some_and(|a| a != "none")
}

fn find_best_progressive_format(
    formats: &[RawFormatJson],
    max_height: Option<u32>,
) -> Option<&RawFormatJson> {
    formats
        .iter()
        .filter(|f| {
            is_progressive(f)
                && f.url.is_some()
                && max_height.is_none_or(|cap| f.height.is_some_and(|h| h <= cap))
        })
        .max_by(|l, r| {
            l.height
                .unwrap_or(0)
                .cmp(&r.height.unwrap_or(0))
                .then_with(|| l.tbr.unwrap_or(0.0).total_cmp(&r.tbr.unwrap_or(0.0)))
        })
}

fn find_best_video_format(
    formats: &[RawFormatJson],
    max_height: Option<u32>,
    allow_split: bool,
) -> Option<&RawFormatJson> {
    formats
        .iter()
        .filter(|f| {
            f.vcodec.as_deref().is_some_and(|v| v != "none")
                && (allow_split || is_progressive(f))
                && max_height.is_none_or(|cap| f.height.is_some_and(|h| h <= cap))
        })
        .max_by(|l, r| {
            l.height
                .unwrap_or(0)
                .cmp(&r.height.unwrap_or(0))
                .then_with(|| l.tbr.unwrap_or(0.0).total_cmp(&r.tbr.unwrap_or(0.0)))
        })
}

fn find_best_audio_format(formats: &[RawFormatJson]) -> Option<&RawFormatJson> {
    formats
        .iter()
        .filter(|f| {
            f.acodec.as_deref().is_some_and(|a| a != "none")
                && f.vcodec.as_deref().unwrap_or("none") == "none"
        })
        .max_by(|l, r| l.tbr.unwrap_or(0.0).total_cmp(&r.tbr.unwrap_or(0.0)))
}

fn map_headers(map: &HashMap<String, String>) -> Vec<(String, String)> {
    map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn resolve_entry_url(base: &Url, raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        Some(trimmed.to_owned())
    } else {
        base.join(trimmed).ok().map(Into::into)
    }
}

/// Parses `RUSCELLO|<done>|<total>|<estimate>` progress lines emitted by `--progress-template`.
pub(crate) fn parse_progress_line(line: &str) -> Option<(u64, Option<u64>)> {
    let rest = line.strip_prefix("RUSCELLO|")?;
    let mut parts = rest.split('|');
    let done = parts
        .next()?
        .trim()
        .parse::<f64>()
        .ok()
        .map(|v| v.max(0.0) as u64)?;
    let total = parts
        .next()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map(|v| v.max(0.0) as u64)
        .or_else(|| {
            parts
                .next()
                .and_then(|s| s.trim().parse::<f64>().ok())
                .map(|v| v.max(0.0) as u64)
        });
    Some((done, total))
}

fn find_single_ready_file(dir: &Path) -> Result<Option<PathBuf>, MediaError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

fn find_latest_regular_file(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .find(|entry| {
            entry.file_type().is_ok_and(|ft| ft.is_file())
                && entry.path().extension().and_then(|e| e.to_str()) != Some("part")
        })
        .map(|entry| entry.path())
}

fn move_file_atomic(src: &Path, dst: &Path) -> Result<(), MediaError> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    let mut input = fs::File::open(src)?;
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    std::io::copy(&mut input, temp.as_file_mut())?;
    temp.as_file().sync_all()?;
    temp.persist(dst).map_err(|error| error.error)?;
    crate::storage::sync_directory(parent)?;
    let _ = fs::remove_file(src);
    Ok(())
}

/// Helper re-exporting sanitized headers for native HTTP fallback callers.
pub(crate) fn extract_cookie_header(
    headers: &reqwest::header::HeaderMap,
) -> (Vec<(String, String)>, Option<String>, Option<String>) {
    let ua = headers
        .get(reqwest::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let (plain, cookie) = split_headers(headers);
    (plain, cookie, ua)
}
