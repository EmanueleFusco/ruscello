use std::{
    fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use m3u8_rs::{AlternativeMediaType, KeyMethod, MediaPlaylist, Playlist};
use reqwest::{Client, Url, header::HeaderMap};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{
    FormatPreset, MediaError, MediaFormat, MediaInspection, MediaKind, MediaPhase, ffmpeg,
    net::{BandwidthContext, ensure_filename, fetch_bytes, report_progress, title_from_url},
    track::{SegmentEncryption, SegmentSource, TrackDownload, TrackPlan, download_track},
};

/// Parameters for executing a native HLS or DASH download into `ready_dir`.
pub(crate) struct NativeStreamJob<'a> {
    pub client: &'a Client,
    pub url: &'a Url,
    pub headers: &'a HeaderMap,
    pub working_dir: &'a Path,
    pub ready_dir: &'a Path,
    pub filename_hint: Option<&'a str>,
    pub format_selector: Option<&'a str>,
    pub variant_uri: Option<&'a str>,
    pub dash_video_id: Option<&'a str>,
    pub dash_audio_id: Option<&'a str>,
    pub concurrency: usize,
    pub bandwidth: &'a BandwidthContext,
    pub ffmpeg_bin: Option<&'a Path>,
    pub cancellation: &'a CancellationToken,
    pub progress: Option<&'a watch::Sender<ruscello_http::DownloadProgress>>,
    pub on_phase: &'a (dyn Fn(MediaPhase) + Send + Sync),
}

/// Inspects an HLS playlist URL (`.m3u8`) and returns selectable variants/formats.
pub(crate) async fn inspect(
    client: &Client,
    url: &Url,
    headers: &HeaderMap,
    ffmpeg_available: bool,
    cancellation: &CancellationToken,
) -> Result<MediaInspection, MediaError> {
    let bytes = fetch_bytes(client, url, headers, None, cancellation).await?;
    let playlist = parse_playlist(&bytes)?;
    let title = title_from_url(url);

    let (formats, default_ext) = match playlist {
        Playlist::MasterPlaylist(master) => {
            let mut variants: Vec<_> = master
                .variants
                .iter()
                .filter(|variant| !variant.is_i_frame && !variant.uri.trim().is_empty())
                .collect();
            variants.sort_by(|left, right| {
                let left_h = left.resolution.map_or(0, |r| r.height);
                let right_h = right.resolution.map_or(0, |r| r.height);
                right_h
                    .cmp(&left_h)
                    .then_with(|| right.bandwidth.cmp(&left.bandwidth))
            });

            let mut formats = Vec::new();
            let top_needs_merge = variants.first().is_some_and(|v| {
                v.audio
                    .as_ref()
                    .is_some_and(|group| find_audio_uri(&master.alternatives, group).is_some())
            });

            formats.push(MediaFormat {
                id: "best".to_owned(),
                label: "Best (Auto)".to_owned(),
                preset: FormatPreset::Best,
                ext: if top_needs_merge && ffmpeg_available {
                    "mp4".to_owned()
                } else {
                    "ts".to_owned()
                },
                height: variants
                    .first()
                    .and_then(|v| v.resolution.map(|r| r.height as u32)),
                width: variants
                    .first()
                    .and_then(|v| v.resolution.map(|r| r.width as u32)),
                bandwidth_bps: variants.first().map(|v| v.bandwidth),
                filesize: None,
                vcodec: variants.first().and_then(|v| v.codecs.clone()),
                acodec: None,
                has_video: true,
                has_audio: true,
                needs_merge: top_needs_merge,
                url: variants
                    .first()
                    .and_then(|v| url.join(&v.uri).ok())
                    .map(Into::into),
                audio_url: None,
                audio_format_id: None,
                headers: Vec::new(),
            });

            for variant in variants {
                let Ok(resolved) = url.join(&variant.uri) else {
                    continue;
                };
                let height = variant.resolution.map(|r| r.height as u32);
                let width = variant.resolution.map(|r| r.width as u32);
                let mbps = variant.bandwidth as f64 / 1_000_000.0;
                let res_label = match (width, height) {
                    (Some(_), Some(h)) => format!("{h}p ({mbps:.1} Mbps)"),
                    _ => format!("{mbps:.1} Mbps"),
                };
                let audio_uri = variant
                    .audio
                    .as_deref()
                    .and_then(|group| find_audio_uri(&master.alternatives, group))
                    .and_then(|raw| url.join(raw).ok());
                let needs_merge = audio_uri.is_some();
                formats.push(MediaFormat {
                    id: resolved.as_str().to_owned(),
                    label: res_label,
                    preset: height.map_or(
                        FormatPreset::FormatId(resolved.as_str().to_owned()),
                        FormatPreset::MaxHeight,
                    ),
                    ext: if needs_merge && ffmpeg_available {
                        "mp4".to_owned()
                    } else {
                        "ts".to_owned()
                    },
                    height,
                    width,
                    bandwidth_bps: Some(variant.bandwidth),
                    filesize: None,
                    vcodec: variant.codecs.clone(),
                    acodec: None,
                    has_video: true,
                    has_audio: !needs_merge || ffmpeg_available,
                    needs_merge,
                    url: Some(resolved.as_str().to_owned()),
                    audio_url: audio_uri.map(Into::into),
                    audio_format_id: variant.audio.clone(),
                    headers: Vec::new(),
                });
            }

            if let Some(audio) = master
                .alternatives
                .iter()
                .find(|alt| alt.media_type == AlternativeMediaType::Audio && alt.uri.is_some())
                && let Some(raw_uri) = audio.uri.as_deref()
                && let Ok(audio_url) = url.join(raw_uri)
            {
                formats.push(MediaFormat {
                    id: "audio".to_owned(),
                    label: format!("Audio Only ({})", audio.name),
                    preset: FormatPreset::AudioOnly { mp3: false },
                    ext: "aac".to_owned(),
                    height: None,
                    width: None,
                    bandwidth_bps: None,
                    filesize: None,
                    vcodec: None,
                    acodec: None,
                    has_video: false,
                    has_audio: true,
                    needs_merge: false,
                    url: Some(audio_url.as_str().to_owned()),
                    audio_url: None,
                    audio_format_id: Some(audio.group_id.clone()),
                    headers: Vec::new(),
                });
            }

            let ext = formats
                .first()
                .map_or_else(|| "ts".to_owned(), |f| f.ext.clone());
            (formats, ext)
        }
        Playlist::MediaPlaylist(media) => {
            let uses_fmp4 = media.segments.iter().any(|s| s.map.is_some());
            let ext = if uses_fmp4 { "mp4" } else { "ts" };
            (
                vec![MediaFormat {
                    id: "best".to_owned(),
                    label: "Best (Stream)".to_owned(),
                    preset: FormatPreset::Best,
                    ext: ext.to_owned(),
                    height: None,
                    width: None,
                    bandwidth_bps: None,
                    filesize: None,
                    vcodec: None,
                    acodec: None,
                    has_video: true,
                    has_audio: true,
                    needs_merge: false,
                    url: Some(url.as_str().to_owned()),
                    audio_url: None,
                    audio_format_id: None,
                    headers: Vec::new(),
                }],
                ext.to_owned(),
            )
        }
    };

    Ok(MediaInspection {
        source_url: url.as_str().to_owned(),
        kind: MediaKind::Hls,
        title: title.clone(),
        suggested_filename: ensure_filename(&title, &default_ext),
        ffmpeg_available,
        formats,
        playlist_title: None,
        playlist_items: Vec::new(),
    })
}

/// Downloads an HLS stream (master or media playlist), handling AES-128 decryption, resume, and concatenation.
pub(crate) async fn download(job: NativeStreamJob<'_>) -> Result<PathBuf, MediaError> {
    let bytes = fetch_bytes(job.client, job.url, job.headers, None, job.cancellation).await?;
    let root_playlist = parse_playlist(&bytes)?;

    let preset = job
        .format_selector
        .map(FormatPreset::parse)
        .unwrap_or_default();

    let (video_url, audio_url) = match root_playlist {
        Playlist::MasterPlaylist(master) => select_master_streams(
            job.url,
            &master,
            &preset,
            job.variant_uri,
            job.ffmpeg_bin.is_some(),
        )?,
        Playlist::MediaPlaylist(_) => (job.url.clone(), None),
    };

    let video_bytes = if video_url == *job.url {
        bytes
    } else {
        fetch_bytes(job.client, &video_url, job.headers, None, job.cancellation).await?
    };
    let Playlist::MediaPlaylist(video_media) = parse_playlist(&video_bytes)? else {
        return Err(MediaError::ManifestParse(
            "expected HLS media playlist from selected variant".to_owned(),
        ));
    };

    let video_plan = build_track_plan(&video_url, &video_media)?;
    let uses_fmp4 = video_plan.init.is_some();

    let audio_plan = if let Some(ref a_url) = audio_url
        && job.ffmpeg_bin.is_some()
        && !matches!(preset, FormatPreset::AudioOnly { .. })
    {
        let audio_bytes =
            fetch_bytes(job.client, a_url, job.headers, None, job.cancellation).await?;
        let Playlist::MediaPlaylist(audio_media) = parse_playlist(&audio_bytes)? else {
            return Err(MediaError::ManifestParse(
                "expected HLS media playlist for audio rendition".to_owned(),
            ));
        };
        Some((a_url.clone(), build_track_plan(a_url, &audio_media)?))
    } else {
        None
    };

    let output_ext = if matches!(preset, FormatPreset::AudioOnly { .. }) {
        if uses_fmp4 { "m4a" } else { "ts" }
    } else if audio_plan.is_some() || uses_fmp4 {
        "mp4"
    } else {
        "ts"
    };
    let base_name = job
        .filename_hint
        .map(str::to_owned)
        .unwrap_or_else(|| title_from_url(job.url));
    let final_filename = ensure_filename(&base_name, output_ext);
    let final_ready_path = job.ready_dir.join(&final_filename);
    fs::create_dir_all(job.ready_dir)?;

    let staging_root = job.working_dir.join("hls");
    fs::create_dir_all(&staging_root)?;

    let video_bytes_acc = Arc::new(AtomicU64::new(0));
    let audio_bytes_acc = Arc::new(AtomicU64::new(0));

    let v_progress: Arc<dyn Fn(u64, Option<u64>) + Send + Sync> = {
        let sender = job.progress.cloned();
        let v_acc = video_bytes_acc.clone();
        let a_acc = audio_bytes_acc.clone();
        Arc::new(move |done, total| {
            v_acc.store(done, Ordering::Relaxed);
            let combined = done + a_acc.load(Ordering::Relaxed);
            report_progress(sender.as_ref(), combined, total);
        })
    };

    if let Some((_, a_plan)) = audio_plan
        && let Some(ffmpeg_bin) = job.ffmpeg_bin
    {
        let video_concat = staging_root.join("video-track.bin");
        let audio_concat = staging_root.join("audio-track.bin");

        download_track(TrackDownload {
            client: job.client,
            headers: job.headers,
            plan: &video_plan,
            staging_dir: &staging_root.join("video"),
            output_file: &video_concat,
            concurrency: job.concurrency,
            bandwidth: job.bandwidth,
            cancellation: job.cancellation,
            progress: v_progress,
        })
        .await?;

        let a_progress: Arc<dyn Fn(u64, Option<u64>) + Send + Sync> = {
            let sender = job.progress.cloned();
            let v_acc = video_bytes_acc.clone();
            let a_acc = audio_bytes_acc.clone();
            Arc::new(move |done, _| {
                a_acc.store(done, Ordering::Relaxed);
                let combined = v_acc.load(Ordering::Relaxed) + done;
                report_progress(sender.as_ref(), combined, None);
            })
        };

        download_track(TrackDownload {
            client: job.client,
            headers: job.headers,
            plan: &a_plan,
            staging_dir: &staging_root.join("audio"),
            output_file: &audio_concat,
            concurrency: job.concurrency,
            bandwidth: job.bandwidth,
            cancellation: job.cancellation,
            progress: a_progress,
        })
        .await?;

        (job.on_phase)(MediaPhase::PostProcessing);
        ffmpeg::remux_av(
            ffmpeg_bin,
            &video_concat,
            &audio_concat,
            &final_ready_path,
            job.cancellation,
        )
        .await?;
        let _ = fs::remove_file(&video_concat);
        let _ = fs::remove_file(&audio_concat);
    } else {
        if matches!(preset, FormatPreset::AudioOnly { .. }) && audio_url.is_none() {
            tracing::debug!("No separate audio rendition found; downloading primary HLS stream");
        }
        download_track(TrackDownload {
            client: job.client,
            headers: job.headers,
            plan: &video_plan,
            staging_dir: &staging_root.join("video"),
            output_file: &final_ready_path,
            concurrency: job.concurrency,
            bandwidth: job.bandwidth,
            cancellation: job.cancellation,
            progress: v_progress,
        })
        .await?;
    }

    Ok(final_ready_path)
}

fn parse_playlist(bytes: &[u8]) -> Result<Playlist, MediaError> {
    m3u8_rs::parse_playlist_res(bytes)
        .map_err(|error| MediaError::ManifestParse(format!("invalid M3U8 playlist: {error}")))
}

fn find_audio_uri<'a>(
    alternatives: &'a [m3u8_rs::AlternativeMedia],
    group_id: &str,
) -> Option<&'a str> {
    alternatives
        .iter()
        .find(|alt| {
            alt.media_type == AlternativeMediaType::Audio
                && alt.group_id == group_id
                && alt.default
                && alt.uri.is_some()
        })
        .or_else(|| {
            alternatives.iter().find(|alt| {
                alt.media_type == AlternativeMediaType::Audio
                    && alt.group_id == group_id
                    && alt.uri.is_some()
            })
        })
        .and_then(|alt| alt.uri.as_deref())
}

fn select_master_streams(
    base_url: &Url,
    master: &m3u8_rs::MasterPlaylist,
    preset: &FormatPreset,
    explicit_variant_uri: Option<&str>,
    ffmpeg_available: bool,
) -> Result<(Url, Option<Url>), MediaError> {
    if matches!(preset, FormatPreset::AudioOnly { .. })
        && let Some(audio) = master
            .alternatives
            .iter()
            .find(|alt| {
                alt.media_type == AlternativeMediaType::Audio && alt.default && alt.uri.is_some()
            })
            .or_else(|| {
                master
                    .alternatives
                    .iter()
                    .find(|alt| alt.media_type == AlternativeMediaType::Audio && alt.uri.is_some())
            })
        && let Some(uri) = audio.uri.as_deref()
    {
        return Ok((join_url(base_url, uri)?, None));
    }

    let mut variants: Vec<_> = master
        .variants
        .iter()
        .filter(|variant| !variant.is_i_frame && !variant.uri.trim().is_empty())
        .collect();
    if variants.is_empty() {
        return Err(MediaError::NoSuitableFormat);
    }

    variants.sort_by(|left, right| {
        let left_h = left.resolution.map_or(0, |r| r.height);
        let right_h = right.resolution.map_or(0, |r| r.height);
        right_h
            .cmp(&left_h)
            .then_with(|| right.bandwidth.cmp(&left.bandwidth))
    });

    if let Some(target) = explicit_variant_uri
        && let Some(variant) = variants.iter().find(|v| {
            v.uri == target
                || base_url
                    .join(&v.uri)
                    .is_ok_and(|resolved| resolved.as_str() == target)
        })
    {
        return resolve_variant_pair(base_url, master, variant);
    }

    let chosen = match preset {
        FormatPreset::Best => {
            if ffmpeg_available {
                variants.first().copied()
            } else {
                variants
                    .iter()
                    .find(|v| {
                        v.audio
                            .as_deref()
                            .is_none_or(|g| find_audio_uri(&master.alternatives, g).is_none())
                    })
                    .or_else(|| variants.first())
                    .copied()
            }
        }
        FormatPreset::MaxHeight(max_h) => {
            let cap = *max_h as u64;
            variants
                .iter()
                .find(|v| v.resolution.is_some_and(|r| r.height <= cap))
                .or_else(|| variants.last())
                .copied()
        }
        FormatPreset::FormatId(id) => variants
            .iter()
            .find(|v| {
                v.uri == *id
                    || base_url
                        .join(&v.uri)
                        .is_ok_and(|resolved| resolved.as_str() == id)
            })
            .or_else(|| variants.first())
            .copied(),
        FormatPreset::AudioOnly { .. } => variants.last().copied(),
    }
    .ok_or(MediaError::NoSuitableFormat)?;

    resolve_variant_pair(base_url, master, chosen)
}

fn resolve_variant_pair(
    base_url: &Url,
    master: &m3u8_rs::MasterPlaylist,
    variant: &m3u8_rs::VariantStream,
) -> Result<(Url, Option<Url>), MediaError> {
    let video = join_url(base_url, &variant.uri)?;
    let audio = variant
        .audio
        .as_deref()
        .and_then(|group| find_audio_uri(&master.alternatives, group))
        .map(|raw| join_url(base_url, raw))
        .transpose()?;
    Ok((video, audio))
}

fn build_track_plan(base_url: &Url, media: &MediaPlaylist) -> Result<TrackPlan, MediaError> {
    if media.segments.is_empty() {
        return Err(MediaError::UnsupportedManifest(
            "HLS playlist contains no segments".to_owned(),
        ));
    }
    let mut current_key: Option<m3u8_rs::Key> = None;
    let mut current_map: Option<m3u8_rs::Map> = None;
    let mut next_offset: u64 = 0;

    let mut init_source = None;
    let mut segments = Vec::with_capacity(media.segments.len());

    for (idx, seg) in media.segments.iter().enumerate() {
        if let Some(ref map) = seg.map {
            current_map = Some(map.clone());
            if init_source.is_none() {
                let map_url = join_url(base_url, &map.uri)?;
                let range = map.byte_range.as_ref().map(|br| {
                    let start = br.offset.unwrap_or(0);
                    start..start + br.length
                });
                init_source = Some(SegmentSource {
                    url: map_url,
                    range,
                    encryption: SegmentEncryption::None,
                });
            }
        }
        let _ = &current_map;

        if let Some(ref key) = seg.key {
            current_key = Some(key.clone());
        }

        let seg_url = join_url(base_url, &seg.uri)?;
        let range: Option<Range<u64>> = seg.byte_range.as_ref().map(|br| {
            let start = br.offset.unwrap_or(next_offset);
            let end = start + br.length;
            next_offset = end;
            start..end
        });

        let encryption = match current_key.as_ref().map(|k| &k.method) {
            None | Some(KeyMethod::None) => SegmentEncryption::None,
            Some(KeyMethod::AES128) => {
                let key = current_key.as_ref().ok_or_else(|| {
                    MediaError::Decryption("missing AES-128 key declaration".to_owned())
                })?;
                let key_uri = key.uri.as_deref().ok_or_else(|| {
                    MediaError::Decryption("AES-128 key missing URI attribute".to_owned())
                })?;
                let key_url = join_url(base_url, key_uri)?;
                let seq = (media.media_sequence as u128) + (idx as u128);
                let iv = parse_iv(key.iv.as_deref(), seq)?;
                SegmentEncryption::Aes128 { key_url, iv }
            }
            Some(KeyMethod::SampleAES) => {
                return Err(MediaError::UnsupportedManifest(
                    "SAMPLE-AES encrypted HLS streams are not supported".to_owned(),
                ));
            }
            Some(KeyMethod::Other(method)) => {
                return Err(MediaError::UnsupportedManifest(format!(
                    "unsupported HLS encryption method: {method}"
                )));
            }
        };

        segments.push(SegmentSource {
            url: seg_url,
            range,
            encryption,
        });
    }

    Ok(TrackPlan {
        key: base_url.as_str().to_owned(),
        init: init_source,
        segments,
        estimated_bytes: None,
    })
}

fn parse_iv(iv_attr: Option<&str>, sequence: u128) -> Result<[u8; 16], MediaError> {
    let Some(raw) = iv_attr else {
        return Ok(sequence.to_be_bytes());
    };
    let trimmed = raw
        .trim()
        .strip_prefix("0x")
        .or_else(|| raw.trim().strip_prefix("0X"))
        .unwrap_or(raw.trim());
    if trimmed.len() > 32 {
        return Err(MediaError::Decryption(format!(
            "invalid HLS AES-128 IV: {raw}"
        )));
    }
    let padded = format!("{trimmed:0>32}");
    let decoded = hex::decode(padded)
        .map_err(|_| MediaError::Decryption(format!("invalid hex in HLS IV: {raw}")))?;
    decoded
        .try_into()
        .map_err(|_| MediaError::Decryption("HLS AES-128 IV must be 16 bytes".to_owned()))
}

fn join_url(base: &Url, relative: &str) -> Result<Url, MediaError> {
    let joined = base
        .join(relative.trim())
        .map_err(|_| MediaError::InvalidUrl(relative.to_owned()))?;
    if !matches!(joined.scheme(), "http" | "https") {
        return Err(MediaError::InvalidUrl(joined.as_str().to_owned()));
    }
    Ok(joined)
}
