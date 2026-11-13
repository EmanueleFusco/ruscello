use std::{
    num::NonZeroU64,
    path::{Path, PathBuf},
};

use ruscello_core::TorrentSource;
use ruscello_http::Url;
use ruscello_storage::{QueueEntry, TorrentQueueState};
use ruscello_torrent::{AddTorrentRequest, TorrentError, TorrentSettings};

pub(super) fn meta_file_path(torrent_data_dir: &Path, id: u64) -> PathBuf {
    torrent_data_dir.join("meta").join(format!("{id}.torrent"))
}

pub(super) async fn write_cached_torrent(
    torrent_data_dir: &Path,
    id: u64,
    bytes: &[u8],
) -> Result<(), TorrentError> {
    let path = meta_file_path(torrent_data_dir, id);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, bytes).await?;
    Ok(())
}

pub(super) async fn read_cached_torrent(torrent_data_dir: &Path, id: u64) -> Option<Vec<u8>> {
    tokio::fs::read(meta_file_path(torrent_data_dir, id))
        .await
        .ok()
}

pub(super) async fn remove_cached_torrent(torrent_data_dir: &Path, id: u64) {
    let _ = tokio::fs::remove_file(meta_file_path(torrent_data_dir, id)).await;
}

pub(super) fn category_target(
    source: &TorrentSource,
    hint: Option<&str>,
) -> (String, Option<Url>, Option<String>) {
    match source {
        TorrentSource::File(path) => {
            let derived = hint.map(str::to_owned).or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_owned)
                    .or_else(|| path.file_name().and_then(|s| s.to_str()).map(str::to_owned))
            });
            (path.to_string_lossy().into_owned(), None, derived)
        }
        TorrentSource::Url(raw) => {
            let parsed = Url::parse(raw).ok();
            let derived = hint.map(str::to_owned).or_else(|| {
                parsed.as_ref().and_then(|url| {
                    url.path_segments()
                        .and_then(|mut segs| segs.next_back())
                        .filter(|seg| !seg.is_empty())
                        .map(percent_decode)
                })
            });
            (raw.clone(), parsed, derived)
        }
        TorrentSource::Magnet(uri) => {
            let derived = hint.map(str::to_owned).or_else(|| magnet_display_name(uri));
            (uri.clone(), None, derived)
        }
    }
}

fn percent_decode(raw: &str) -> String {
    percent_encoding::percent_decode_str(raw)
        .decode_utf8_lossy()
        .into_owned()
}

pub(super) fn magnet_display_name(uri: &str) -> Option<String> {
    let query = uri.split_once('?')?.1;
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=')
            && key.eq_ignore_ascii_case("dn")
            && !value.is_empty()
        {
            let plus_as_space = value.replace('+', " ");
            return Some(percent_decode(&plus_as_space));
        }
    }
    None
}

pub(super) fn effective_settings(
    base: &TorrentSettings,
    global_bandwidth: Option<NonZeroU64>,
) -> TorrentSettings {
    let mut effective = base.clone();
    effective.download_limit_bps = match (base.download_limit_bps, global_bandwidth) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    effective
}

pub(super) fn build_request(
    entry: &QueueEntry,
    source: &TorrentSource,
    cached_bytes: Option<Vec<u8>>,
    category_bandwidth: Option<NonZeroU64>,
    force_recheck: bool,
) -> AddTorrentRequest {
    let torrent_state = entry.torrent.clone().unwrap_or_default();
    let download_limit_bps = match (entry.download.bandwidth, category_bandwidth) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    AddTorrentRequest {
        id: entry.download.id,
        source: source.clone(),
        destination: entry.download.destination.clone(),
        resolved_output_folder: torrent_state.output_folder,
        cached_torrent_bytes: cached_bytes,
        only_files: torrent_state.only_files,
        paused: false,
        force_recheck,
        prior_uploaded_bytes: torrent_state.uploaded_bytes,
        prior_seed_elapsed_secs: torrent_state.seed_elapsed_secs,
        seed_ratio_millis: torrent_state.seed_ratio_millis,
        seed_time_limit_minutes: torrent_state.seed_time_limit_minutes,
        download_limit_bps,
        initial_peers: torrent_state.initial_peers,
    }
}

pub(super) fn ensure_torrent_state(entry: &mut QueueEntry) -> &mut TorrentQueueState {
    entry.torrent.get_or_insert_with(TorrentQueueState::default)
}
