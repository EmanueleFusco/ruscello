use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};
use reqwest::{Client, Url, header::HeaderMap};

use crate::{
    FormatPreset, MediaError, MediaFormat, MediaInspection, MediaKind, MediaPhase, ffmpeg,
    hls::NativeStreamJob,
    net::{ensure_filename, fetch_bytes, report_progress, title_from_url},
    track::{SegmentEncryption, SegmentSource, TrackDownload, TrackPlan, download_track},
};

#[derive(Debug, Default, Clone)]
struct MpdDocument {
    duration_secs: Option<f64>,
    base_url: Option<String>,
    periods: Vec<Period>,
}

#[derive(Debug, Default, Clone)]
struct Period {
    base_url: Option<String>,
    segment_template: Option<SegmentTemplate>,
    segment_list: Option<SegmentList>,
    adaptation_sets: Vec<AdaptationSet>,
}

#[derive(Debug, Default, Clone)]
struct AdaptationSet {
    mime_type: Option<String>,
    content_type: Option<String>,
    codecs: Option<String>,
    lang: Option<String>,
    base_url: Option<String>,
    segment_template: Option<SegmentTemplate>,
    segment_list: Option<SegmentList>,
    representations: Vec<Representation>,
}

#[derive(Debug, Default, Clone)]
struct Representation {
    id: String,
    bandwidth: u64,
    width: Option<u32>,
    height: Option<u32>,
    codecs: Option<String>,
    mime_type: Option<String>,
    base_url: Option<String>,
    segment_template: Option<SegmentTemplate>,
    segment_list: Option<SegmentList>,
}

#[derive(Debug, Default, Clone)]
struct SegmentTemplate {
    initialization: Option<String>,
    media: Option<String>,
    start_number: Option<u64>,
    duration: Option<u64>,
    timescale: Option<u64>,
    timeline: Vec<TimelineEntry>,
}

#[derive(Debug, Default, Clone, Copy)]
struct TimelineEntry {
    t: Option<u64>,
    d: u64,
    r: i64,
}

#[derive(Debug, Default, Clone)]
struct SegmentList {
    initialization: Option<String>,
    segments: Vec<String>,
}

#[derive(Debug, Clone)]
struct ResolvedRep {
    id: String,
    bandwidth: u64,
    width: Option<u32>,
    height: Option<u32>,
    codecs: Option<String>,
    mime_type: Option<String>,
    lang: Option<String>,
    plan: TrackPlan,
    is_audio: bool,
    is_video: bool,
}

/// Inspects an MPEG-DASH `.mpd` manifest URL and lists video/audio representations.
pub(crate) async fn inspect(
    client: &Client,
    url: &Url,
    headers: &HeaderMap,
    ffmpeg_available: bool,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<MediaInspection, MediaError> {
    let bytes = fetch_bytes(client, url, headers, None, cancellation).await?;
    let mpd = parse_mpd(&bytes)?;
    let reps = resolve_representations(url, &mpd)?;
    let (videos, audios) = split_tracks(&reps);
    let title = title_from_url(url);
    let has_split_av = !videos.is_empty() && !audios.is_empty();

    let mut formats = Vec::new();
    let best_video = videos.first();
    let best_audio = audios.first();

    let default_ext = if let Some(v) = best_video {
        ext_from_mime(v.mime_type.as_deref(), true)
    } else if let Some(a) = best_audio {
        ext_from_mime(a.mime_type.as_deref(), false)
    } else {
        "mp4"
    };

    if let Some(best_v) = best_video {
        formats.push(MediaFormat {
            id: "best".to_owned(),
            label: "Best (Auto)".to_owned(),
            preset: FormatPreset::Best,
            ext: ext_from_mime(best_v.mime_type.as_deref(), true).to_owned(),
            height: best_v.height,
            width: best_v.width,
            bandwidth_bps: Some(best_v.bandwidth + best_audio.map_or(0, |a| a.bandwidth)),
            filesize: best_v.plan.estimated_bytes,
            vcodec: best_v.codecs.clone(),
            acodec: best_audio.and_then(|a| a.codecs.clone()),
            has_video: true,
            has_audio: !has_split_av || ffmpeg_available,
            needs_merge: has_split_av,
            url: Some(url.as_str().to_owned()),
            audio_url: None,
            audio_format_id: best_audio.map(|a| a.id.clone()),
            headers: Vec::new(),
        });
    }

    for v in &videos {
        let mbps = v.bandwidth as f64 / 1_000_000.0;
        let label = match (v.width, v.height) {
            (Some(_), Some(h)) => format!("{h}p ({mbps:.1} Mbps)"),
            _ => format!("{} ({mbps:.1} Mbps)", v.id),
        };
        formats.push(MediaFormat {
            id: v.id.clone(),
            label,
            preset: v.height.map_or_else(
                || FormatPreset::FormatId(v.id.clone()),
                FormatPreset::MaxHeight,
            ),
            ext: ext_from_mime(v.mime_type.as_deref(), true).to_owned(),
            height: v.height,
            width: v.width,
            bandwidth_bps: Some(v.bandwidth),
            filesize: v.plan.estimated_bytes,
            vcodec: v.codecs.clone(),
            acodec: best_audio.and_then(|a| a.codecs.clone()),
            has_video: true,
            has_audio: !has_split_av || ffmpeg_available,
            needs_merge: has_split_av,
            url: Some(url.as_str().to_owned()),
            audio_url: None,
            audio_format_id: best_audio.map(|a| a.id.clone()),
            headers: Vec::new(),
        });
    }

    if let Some(a) = best_audio {
        let kbps = a.bandwidth / 1000;
        let lang_suffix = a
            .lang
            .as_deref()
            .map(|l| format!(" [{l}]"))
            .unwrap_or_default();
        formats.push(MediaFormat {
            id: "audio".to_owned(),
            label: format!("Audio Only{lang_suffix} ({kbps} kbps)"),
            preset: FormatPreset::AudioOnly { mp3: false },
            ext: ext_from_mime(a.mime_type.as_deref(), false).to_owned(),
            height: None,
            width: None,
            bandwidth_bps: Some(a.bandwidth),
            filesize: a.plan.estimated_bytes,
            vcodec: None,
            acodec: a.codecs.clone(),
            has_video: false,
            has_audio: true,
            needs_merge: false,
            url: Some(url.as_str().to_owned()),
            audio_url: None,
            audio_format_id: Some(a.id.clone()),
            headers: Vec::new(),
        });
    }

    if formats.is_empty() {
        return Err(MediaError::NoSuitableFormat);
    }

    Ok(MediaInspection {
        source_url: url.as_str().to_owned(),
        kind: MediaKind::Dash,
        title: title.clone(),
        suggested_filename: ensure_filename(&title, default_ext),
        ffmpeg_available,
        formats,
        playlist_title: None,
        playlist_items: Vec::new(),
    })
}

/// Downloads a DASH MPD stream: separate video and audio tracks, optional `ffmpeg` remux.
pub(crate) async fn download(job: NativeStreamJob<'_>) -> Result<PathBuf, MediaError> {
    let bytes = fetch_bytes(job.client, job.url, job.headers, None, job.cancellation).await?;
    let mpd = parse_mpd(&bytes)?;
    let reps = resolve_representations(job.url, &mpd)?;
    let (videos, audios) = split_tracks(&reps);

    let preset = job
        .format_selector
        .map(FormatPreset::parse)
        .unwrap_or_default();

    let chosen_audio = job
        .dash_audio_id
        .and_then(|id| audios.iter().find(|a| a.id == id))
        .or_else(|| audios.first());

    let audio_only = matches!(preset, FormatPreset::AudioOnly { .. }) || videos.is_empty();
    let base_name = job
        .filename_hint
        .map(str::to_owned)
        .unwrap_or_else(|| title_from_url(job.url));

    fs::create_dir_all(job.ready_dir)?;
    let staging_root = job.working_dir.join("dash");
    fs::create_dir_all(&staging_root)?;

    let video_acc = Arc::new(AtomicU64::new(0));
    let audio_acc = Arc::new(AtomicU64::new(0));

    if audio_only {
        let audio_rep = chosen_audio.ok_or(MediaError::NoSuitableFormat)?;
        let ext = ext_from_mime(audio_rep.mime_type.as_deref(), false);
        let ready_path = job.ready_dir.join(ensure_filename(&base_name, ext));
        let sender = job.progress.cloned();
        let cb: Arc<dyn Fn(u64, Option<u64>) + Send + Sync> =
            Arc::new(move |done, total| report_progress(sender.as_ref(), done, total));
        download_track(TrackDownload {
            client: job.client,
            headers: job.headers,
            plan: &audio_rep.plan,
            staging_dir: &staging_root.join("audio"),
            output_file: &ready_path,
            concurrency: job.concurrency,
            bandwidth: job.bandwidth,
            cancellation: job.cancellation,
            progress: cb,
        })
        .await?;
        return Ok(ready_path);
    }

    let chosen_video =
        select_video(&videos, &preset, job.dash_video_id).ok_or(MediaError::NoSuitableFormat)?;
    let ext = ext_from_mime(chosen_video.mime_type.as_deref(), true);
    let ready_path = job.ready_dir.join(ensure_filename(&base_name, ext));

    let v_progress: Arc<dyn Fn(u64, Option<u64>) + Send + Sync> = {
        let sender = job.progress.cloned();
        let v = video_acc.clone();
        let a = audio_acc.clone();
        Arc::new(move |done, total| {
            v.store(done, Ordering::Relaxed);
            let combined = done + a.load(Ordering::Relaxed);
            report_progress(sender.as_ref(), combined, total);
        })
    };

    if let (Some(audio_rep), Some(ffmpeg_bin)) = (chosen_audio, job.ffmpeg_bin) {
        let video_track = staging_root.join("video-track.bin");
        let audio_track = staging_root.join("audio-track.bin");

        download_track(TrackDownload {
            client: job.client,
            headers: job.headers,
            plan: &chosen_video.plan,
            staging_dir: &staging_root.join("video"),
            output_file: &video_track,
            concurrency: job.concurrency,
            bandwidth: job.bandwidth,
            cancellation: job.cancellation,
            progress: v_progress,
        })
        .await?;

        let a_progress: Arc<dyn Fn(u64, Option<u64>) + Send + Sync> = {
            let sender = job.progress.cloned();
            let v = video_acc.clone();
            let a = audio_acc.clone();
            Arc::new(move |done, _| {
                a.store(done, Ordering::Relaxed);
                let combined = v.load(Ordering::Relaxed) + done;
                report_progress(sender.as_ref(), combined, None);
            })
        };

        download_track(TrackDownload {
            client: job.client,
            headers: job.headers,
            plan: &audio_rep.plan,
            staging_dir: &staging_root.join("audio"),
            output_file: &audio_track,
            concurrency: job.concurrency,
            bandwidth: job.bandwidth,
            cancellation: job.cancellation,
            progress: a_progress,
        })
        .await?;

        (job.on_phase)(MediaPhase::PostProcessing);
        ffmpeg::remux_av(
            ffmpeg_bin,
            &video_track,
            &audio_track,
            &ready_path,
            job.cancellation,
        )
        .await?;
        let _ = fs::remove_file(&video_track);
        let _ = fs::remove_file(&audio_track);
    } else {
        if chosen_audio.is_some() {
            tracing::debug!("ffmpeg sidecar unavailable; downloading DASH video track directly");
        }
        download_track(TrackDownload {
            client: job.client,
            headers: job.headers,
            plan: &chosen_video.plan,
            staging_dir: &staging_root.join("video"),
            output_file: &ready_path,
            concurrency: job.concurrency,
            bandwidth: job.bandwidth,
            cancellation: job.cancellation,
            progress: v_progress,
        })
        .await?;
    }

    Ok(ready_path)
}

fn select_video<'a>(
    videos: &[&'a ResolvedRep],
    preset: &FormatPreset,
    explicit_id: Option<&str>,
) -> Option<&'a ResolvedRep> {
    if let Some(id) = explicit_id
        && let Some(rep) = videos.iter().find(|r| r.id == id)
    {
        return Some(*rep);
    }
    match preset {
        FormatPreset::Best => videos.first().copied(),
        FormatPreset::MaxHeight(max_h) => videos
            .iter()
            .find(|r| r.height.is_some_and(|h| h <= *max_h))
            .or_else(|| videos.last())
            .copied(),
        FormatPreset::FormatId(id) => videos
            .iter()
            .find(|r| r.id == *id)
            .or_else(|| videos.first())
            .copied(),
        FormatPreset::AudioOnly { .. } => videos.last().copied(),
    }
}

fn split_tracks(reps: &[ResolvedRep]) -> (Vec<&ResolvedRep>, Vec<&ResolvedRep>) {
    let mut videos: Vec<_> = reps.iter().filter(|r| r.is_video).collect();
    let mut audios: Vec<_> = reps.iter().filter(|r| r.is_audio).collect();
    videos.sort_by(|l, r| {
        r.height
            .unwrap_or(0)
            .cmp(&l.height.unwrap_or(0))
            .then_with(|| r.bandwidth.cmp(&l.bandwidth))
    });
    audios.sort_by_key(|l| std::cmp::Reverse(l.bandwidth));
    (videos, audios)
}

fn resolve_representations(
    base_url: &Url,
    mpd: &MpdDocument,
) -> Result<Vec<ResolvedRep>, MediaError> {
    let mut out = Vec::new();
    let mpd_base = resolve_opt_base(base_url, mpd.base_url.as_deref())?;
    for period in &mpd.periods {
        let period_base = resolve_opt_base(&mpd_base, period.base_url.as_deref())?;
        for adaptation in &period.adaptation_sets {
            let adapt_base = resolve_opt_base(&period_base, adaptation.base_url.as_deref())?;
            for rep in &adaptation.representations {
                let rep_base = resolve_opt_base(&adapt_base, rep.base_url.as_deref())?;
                let mime = rep
                    .mime_type
                    .clone()
                    .or_else(|| adaptation.mime_type.clone());
                let content_type = adaptation.content_type.as_deref();
                let is_audio = content_type == Some("audio")
                    || mime.as_deref().is_some_and(|m| m.starts_with("audio/"));
                let is_video = content_type == Some("video")
                    || mime.as_deref().is_some_and(|m| m.starts_with("video/"))
                    || (!is_audio && rep.height.is_some());

                let merged_template = merge_template(
                    period.segment_template.as_ref(),
                    adaptation.segment_template.as_ref(),
                    rep.segment_template.as_ref(),
                );
                let merged_list = rep
                    .segment_list
                    .as_ref()
                    .or(adaptation.segment_list.as_ref())
                    .or(period.segment_list.as_ref());

                let plan = build_rep_plan(
                    &rep_base,
                    rep,
                    mpd.duration_secs,
                    merged_template.as_ref(),
                    merged_list,
                )?;
                out.push(ResolvedRep {
                    id: rep.id.clone(),
                    bandwidth: rep.bandwidth,
                    width: rep.width,
                    height: rep.height,
                    codecs: rep.codecs.clone().or_else(|| adaptation.codecs.clone()),
                    mime_type: mime,
                    lang: adaptation.lang.clone(),
                    plan,
                    is_audio,
                    is_video,
                });
            }
        }
    }
    Ok(out)
}

fn build_rep_plan(
    rep_base: &Url,
    rep: &Representation,
    duration_secs: Option<f64>,
    template: Option<&SegmentTemplate>,
    list: Option<&SegmentList>,
) -> Result<TrackPlan, MediaError> {
    if let Some(tpl) = template {
        let init = tpl
            .initialization
            .as_deref()
            .map(|raw| expand_template(raw, &rep.id, rep.bandwidth, 0, 0))
            .map(|rel| join_url(rep_base, &rel))
            .transpose()?
            .map(|url| SegmentSource {
                url,
                range: None,
                encryption: SegmentEncryption::None,
            });

        let media_tpl = tpl.media.as_deref().ok_or_else(|| {
            MediaError::ManifestParse(format!("SegmentTemplate for {} missing @media", rep.id))
        })?;
        let start_num = tpl.start_number.unwrap_or(1);
        let mut segments = Vec::new();

        if !tpl.timeline.is_empty() {
            let mut cur_time = 0_u64;
            let mut seg_num = start_num;
            for entry in &tpl.timeline {
                if let Some(t) = entry.t {
                    cur_time = t;
                }
                let repeats = if entry.r < 0 { 0 } else { entry.r as u64 };
                for _ in 0..=repeats {
                    let expanded =
                        expand_template(media_tpl, &rep.id, rep.bandwidth, seg_num, cur_time);
                    segments.push(SegmentSource {
                        url: join_url(rep_base, &expanded)?,
                        range: None,
                        encryption: SegmentEncryption::None,
                    });
                    cur_time = cur_time.saturating_add(entry.d);
                    seg_num = seg_num.saturating_add(1);
                }
            }
        } else {
            let timescale = tpl.timescale.unwrap_or(1).max(1) as f64;
            let seg_dur = tpl.duration.unwrap_or(1).max(1) as f64;
            let count = match (duration_secs, tpl.duration) {
                (Some(total_s), Some(_)) => {
                    ((total_s * timescale) / seg_dur).ceil().max(1.0) as u64
                }
                _ => 1,
            };
            for offset in 0..count {
                let seg_num = start_num.saturating_add(offset);
                let seg_time = offset.saturating_mul(tpl.duration.unwrap_or(0));
                let expanded =
                    expand_template(media_tpl, &rep.id, rep.bandwidth, seg_num, seg_time);
                segments.push(SegmentSource {
                    url: join_url(rep_base, &expanded)?,
                    range: None,
                    encryption: SegmentEncryption::None,
                });
            }
        }

        return Ok(TrackPlan {
            key: format!("{rep_base}#{}", rep.id),
            init,
            segments,
            estimated_bytes: duration_secs.map(|secs| ((secs * rep.bandwidth as f64) / 8.0) as u64),
        });
    }

    if let Some(list) = list {
        let init = list
            .initialization
            .as_deref()
            .map(|raw| join_url(rep_base, raw))
            .transpose()?
            .map(|url| SegmentSource {
                url,
                range: None,
                encryption: SegmentEncryption::None,
            });
        let mut segments = Vec::with_capacity(list.segments.len());
        for raw in &list.segments {
            segments.push(SegmentSource {
                url: join_url(rep_base, raw)?,
                range: None,
                encryption: SegmentEncryption::None,
            });
        }
        return Ok(TrackPlan {
            key: format!("{rep_base}#{}", rep.id),
            init,
            segments,
            estimated_bytes: None,
        });
    }

    Ok(TrackPlan {
        key: format!("{rep_base}#{}", rep.id),
        init: None,
        segments: vec![SegmentSource {
            url: rep_base.clone(),
            range: None,
            encryption: SegmentEncryption::None,
        }],
        estimated_bytes: None,
    })
}

fn expand_template(template: &str, rep_id: &str, bandwidth: u64, number: u64, time: u64) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(pos) = rest.find('$') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        let Some(end) = after.find('$') else {
            out.push('$');
            rest = after;
            continue;
        };
        let token = &after[..end];
        rest = &after[end + 1..];
        if token.is_empty() {
            out.push('$');
        } else if token == "RepresentationID" {
            out.push_str(rep_id);
        } else if token == "Bandwidth" {
            out.push_str(&bandwidth.to_string());
        } else if let Some(formatted) = format_numeric_token(token, "Number", number)
            .or_else(|| format_numeric_token(token, "Time", time))
        {
            out.push_str(&formatted);
        } else {
            out.push('$');
            out.push_str(token);
            out.push('$');
        }
    }
    out.push_str(rest);
    out
}

fn format_numeric_token(token: &str, key: &str, value: u64) -> Option<String> {
    if token == key {
        return Some(value.to_string());
    }
    let suffix = token.strip_prefix(key)?.strip_prefix('%')?;
    let digits = suffix.strip_suffix('d')?;
    let width = digits
        .trim_start_matches('0')
        .parse::<usize>()
        .ok()
        .unwrap_or(1);
    Some(format!("{value:0width$}"))
}

fn merge_template(
    period: Option<&SegmentTemplate>,
    adapt: Option<&SegmentTemplate>,
    rep: Option<&SegmentTemplate>,
) -> Option<SegmentTemplate> {
    if period.is_none() && adapt.is_none() && rep.is_none() {
        return None;
    }
    let layers = [period, adapt, rep];
    let mut merged = SegmentTemplate::default();
    for layer in layers.into_iter().flatten() {
        if layer.initialization.is_some() {
            merged.initialization = layer.initialization.clone();
        }
        if layer.media.is_some() {
            merged.media = layer.media.clone();
        }
        if layer.start_number.is_some() {
            merged.start_number = layer.start_number;
        }
        if layer.duration.is_some() {
            merged.duration = layer.duration;
        }
        if layer.timescale.is_some() {
            merged.timescale = layer.timescale;
        }
        if !layer.timeline.is_empty() {
            merged.timeline = layer.timeline.clone();
        }
    }
    Some(merged)
}

fn parse_mpd(bytes: &[u8]) -> Result<MpdDocument, MediaError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| MediaError::ManifestParse(format!("MPD is not UTF-8: {error}")))?;
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);

    let mut mpd = MpdDocument::default();
    let mut cur_period: Option<Period> = None;
    let mut cur_adapt: Option<AdaptationSet> = None;
    let mut cur_rep: Option<Representation> = None;
    let mut cur_tpl: Option<SegmentTemplate> = None;
    let mut cur_list: Option<SegmentList> = None;
    let mut in_base_url = false;
    let mut base_url_text = String::new();
    let mut rep_seq = 0_usize;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let name = local_name(e);
                match name.as_str() {
                    "MPD" => {
                        mpd.duration_secs = attr(e, "mediaPresentationDuration")
                            .as_deref()
                            .and_then(parse_iso_duration);
                    }
                    "Period" => cur_period = Some(Period::default()),
                    "AdaptationSet" => {
                        cur_adapt = Some(AdaptationSet {
                            mime_type: attr(e, "mimeType"),
                            content_type: attr(e, "contentType"),
                            codecs: attr(e, "codecs"),
                            lang: attr(e, "lang"),
                            ..Default::default()
                        });
                    }
                    "Representation" => {
                        rep_seq += 1;
                        let id = attr(e, "id").unwrap_or_else(|| format!("rep-{rep_seq}"));
                        cur_rep = Some(Representation {
                            id,
                            bandwidth: attr_u64(e, "bandwidth").unwrap_or(0),
                            width: attr_u32(e, "width"),
                            height: attr_u32(e, "height"),
                            codecs: attr(e, "codecs"),
                            mime_type: attr(e, "mimeType"),
                            ..Default::default()
                        });
                    }
                    "SegmentTemplate" => {
                        cur_tpl = Some(parse_segment_template_attrs(e));
                    }
                    "SegmentList" => {
                        cur_list = Some(SegmentList::default());
                    }
                    "SegmentTimeline" => {}
                    "S" => {
                        if let Some(ref mut tpl) = cur_tpl {
                            tpl.timeline.push(parse_timeline_entry(e));
                        }
                    }
                    "Initialization" => {
                        if let Some(ref mut list) = cur_list {
                            list.initialization = attr(e, "sourceURL");
                        }
                    }
                    "SegmentURL" => {
                        if let Some(ref mut list) = cur_list
                            && let Some(media) = attr(e, "media")
                        {
                            list.segments.push(media);
                        }
                    }
                    "BaseURL" => {
                        in_base_url = true;
                        base_url_text.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                let name = local_name(e);
                match name.as_str() {
                    "SegmentTemplate" => {
                        let tpl = parse_segment_template_attrs(e);
                        assign_template(&mut cur_rep, &mut cur_adapt, &mut cur_period, tpl);
                    }
                    "S" => {
                        if let Some(ref mut tpl) = cur_tpl {
                            tpl.timeline.push(parse_timeline_entry(e));
                        }
                    }
                    "Initialization" => {
                        if let Some(ref mut list) = cur_list {
                            list.initialization = attr(e, "sourceURL");
                        }
                    }
                    "SegmentURL" => {
                        if let Some(ref mut list) = cur_list
                            && let Some(media) = attr(e, "media")
                        {
                            list.segments.push(media);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) => {
                if in_base_url {
                    let decoded = String::from_utf8_lossy(e.as_ref());
                    base_url_text.push_str(decoded.trim());
                }
            }
            Ok(Event::End(ref e)) => {
                let name = local_name_end(e);
                match name.as_str() {
                    "BaseURL" => {
                        in_base_url = false;
                        if !base_url_text.is_empty() {
                            if let Some(ref mut rep) = cur_rep {
                                rep.base_url = Some(base_url_text.clone());
                            } else if let Some(ref mut adapt) = cur_adapt {
                                adapt.base_url = Some(base_url_text.clone());
                            } else if let Some(ref mut period) = cur_period {
                                period.base_url = Some(base_url_text.clone());
                            } else {
                                mpd.base_url = Some(base_url_text.clone());
                            }
                        }
                    }
                    "SegmentTemplate" => {
                        if let Some(tpl) = cur_tpl.take() {
                            assign_template(&mut cur_rep, &mut cur_adapt, &mut cur_period, tpl);
                        }
                    }
                    "SegmentList" => {
                        if let Some(list) = cur_list.take() {
                            if let Some(ref mut rep) = cur_rep {
                                rep.segment_list = Some(list);
                            } else if let Some(ref mut adapt) = cur_adapt {
                                adapt.segment_list = Some(list);
                            } else if let Some(ref mut period) = cur_period {
                                period.segment_list = Some(list);
                            }
                        }
                    }
                    "Representation" => {
                        if let (Some(adapt), Some(rep)) = (cur_adapt.as_mut(), cur_rep.take()) {
                            adapt.representations.push(rep);
                        }
                    }
                    "AdaptationSet" => {
                        if let (Some(period), Some(adapt)) = (cur_period.as_mut(), cur_adapt.take())
                        {
                            period.adaptation_sets.push(adapt);
                        }
                    }
                    "Period" => {
                        if let Some(period) = cur_period.take() {
                            mpd.periods.push(period);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => {
                return Err(MediaError::ManifestParse(format!(
                    "failed to parse DASH MPD XML: {error}"
                )));
            }
            _ => {}
        }
    }
    if mpd.periods.is_empty() {
        return Err(MediaError::ManifestParse(
            "DASH MPD contains no Period elements".to_owned(),
        ));
    }
    Ok(mpd)
}

fn assign_template(
    rep: &mut Option<Representation>,
    adapt: &mut Option<AdaptationSet>,
    period: &mut Option<Period>,
    tpl: SegmentTemplate,
) {
    if let Some(rep) = rep {
        rep.segment_template = Some(tpl);
    } else if let Some(adapt) = adapt {
        adapt.segment_template = Some(tpl);
    } else if let Some(period) = period {
        period.segment_template = Some(tpl);
    }
}

fn parse_segment_template_attrs(e: &BytesStart<'_>) -> SegmentTemplate {
    SegmentTemplate {
        initialization: attr(e, "initialization"),
        media: attr(e, "media"),
        start_number: attr_u64(e, "startNumber"),
        duration: attr_u64(e, "duration"),
        timescale: attr_u64(e, "timescale"),
        timeline: Vec::new(),
    }
}

fn parse_timeline_entry(e: &BytesStart<'_>) -> TimelineEntry {
    TimelineEntry {
        t: attr_u64(e, "t"),
        d: attr_u64(e, "d").unwrap_or(0),
        r: attr(e, "r")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0),
    }
}

fn local_name(event: &BytesStart<'_>) -> String {
    let raw = String::from_utf8_lossy(event.name().as_ref()).into_owned();
    raw.rsplit(':').next().unwrap_or(&raw).to_owned()
}

fn local_name_end(event: &quick_xml::events::BytesEnd<'_>) -> String {
    let raw = String::from_utf8_lossy(event.name().as_ref()).into_owned();
    raw.rsplit(':').next().unwrap_or(&raw).to_owned()
}

fn attr(e: &BytesStart<'_>, name: &str) -> Option<String> {
    for attribute in e.attributes().flatten() {
        let key = String::from_utf8_lossy(attribute.key.as_ref());
        let local = key.rsplit(':').next().unwrap_or(&key);
        if local == name {
            return String::from_utf8(attribute.value.into_owned()).ok();
        }
    }
    None
}

fn attr_u64(e: &BytesStart<'_>, name: &str) -> Option<u64> {
    attr(e, name)?.parse::<u64>().ok()
}

fn attr_u32(e: &BytesStart<'_>, name: &str) -> Option<u32> {
    attr(e, name)?.parse::<u32>().ok()
}

/// Parses an ISO-8601 duration such as `PT1H2M3.5S` into seconds.
pub(crate) fn parse_iso_duration(raw: &str) -> Option<f64> {
    let rest = raw.trim().strip_prefix('P')?;
    let (_, time_part) = rest.split_once('T')?;
    let mut total = 0.0_f64;
    let mut num = String::new();
    for ch in time_part.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            num.push(ch);
        } else {
            let value = num.parse::<f64>().ok()?;
            num.clear();
            match ch {
                'H' => total += value * 3600.0,
                'M' => total += value * 60.0,
                'S' => total += value,
                _ => return None,
            }
        }
    }
    (total > 0.0).then_some(total)
}

fn ext_from_mime(mime: Option<&str>, is_video: bool) -> &'static str {
    match mime {
        Some(m) if m.contains("webm") => "webm",
        Some(m) if m.contains("mp4") => {
            if is_video {
                "mp4"
            } else {
                "m4a"
            }
        }
        Some(m) if m.contains("mp2t") => "ts",
        _ => {
            if is_video {
                "mp4"
            } else {
                "m4a"
            }
        }
    }
}

fn resolve_opt_base(current: &Url, child: Option<&str>) -> Result<Url, MediaError> {
    match child {
        Some(value) if !value.trim().is_empty() => join_url(current, value),
        _ => Ok(current.clone()),
    }
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

#[cfg(feature = "fuzzing")]
pub(crate) fn fuzz_parse(input: &[u8]) {
    let _ = parse_mpd(input);
}
