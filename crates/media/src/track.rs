use std::{
    collections::HashMap,
    ops::Range,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use aes::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7};
use futures_util::{StreamExt, stream::FuturesUnordered};
use reqwest::{Client, Url, header::HeaderMap};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{
    MediaError,
    net::{BandwidthContext, fetch_bytes, fetch_segment_rate_limited},
    storage::SegmentStore,
};

/// Decryption method applied to a media segment after download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SegmentEncryption {
    None,
    Aes128 { key_url: Url, iv: [u8; 16] },
}

/// One remote segment or init chunk with optional HTTP byte range and encryption.
#[derive(Debug, Clone)]
pub(crate) struct SegmentSource {
    pub url: Url,
    pub range: Option<Range<u64>>,
    pub encryption: SegmentEncryption,
}

/// Describes an entire video or audio elementary/segment track.
#[derive(Debug, Clone)]
pub(crate) struct TrackPlan {
    pub key: String,
    pub init: Option<SegmentSource>,
    pub segments: Vec<SegmentSource>,
    pub estimated_bytes: Option<u64>,
}

/// Options passed into [download_track].
pub(crate) struct TrackDownload<'a> {
    pub client: &'a Client,
    pub headers: &'a HeaderMap,
    pub plan: &'a TrackPlan,
    pub staging_dir: &'a Path,
    pub output_file: &'a Path,
    pub concurrency: usize,
    pub bandwidth: &'a BandwidthContext,
    pub cancellation: &'a CancellationToken,
    pub progress: Arc<dyn Fn(u64, Option<u64>) + Send + Sync>,
}

/// Downloads all missing init/media segments with resume and concatenates them into `output_file`.
pub(crate) async fn download_track(params: TrackDownload<'_>) -> Result<u64, MediaError> {
    let TrackDownload {
        client,
        headers,
        plan,
        staging_dir,
        output_file,
        concurrency,
        bandwidth,
        cancellation,
        progress,
    } = params;
    if plan.segments.is_empty() {
        return Err(MediaError::UnsupportedManifest(
            "media track contains zero segments".to_owned(),
        ));
    }

    let store = {
        let dir = staging_dir.to_path_buf();
        let key = plan.key.clone();
        let count = plan.segments.len();
        tokio::task::spawn_blocking(move || SegmentStore::open(dir, &key, count))
            .await
            .map_err(|error| MediaError::Io(std::io::Error::other(error)))??
    };

    let (init_done, completed, already_bytes) = store.resume_plan()?;
    let completed_count = completed.iter().filter(|done| **done).count();
    let avg_completed_bytes = if completed_count > 0 {
        already_bytes / completed_count as u64
    } else {
        0
    };
    let estimated_total = plan.estimated_bytes.or_else(|| {
        (avg_completed_bytes > 0).then(|| avg_completed_bytes * plan.segments.len() as u64)
    });

    let total_downloaded = Arc::new(AtomicU64::new(already_bytes));
    progress(already_bytes, estimated_total);

    if let Some(init) = &plan.init
        && !init_done
    {
        let on_chunk = {
            let counter = total_downloaded.clone();
            let cb = progress.clone();
            move |delta: u64| {
                let current = counter.fetch_add(delta, Ordering::Relaxed) + delta;
                cb(current, estimated_total);
            }
        };
        let bytes = fetch_segment_rate_limited(
            client,
            &init.url,
            headers,
            init.range.clone(),
            bandwidth,
            cancellation,
            Some(&on_chunk),
        )
        .await?;
        let store_clone = store.clone();
        tokio::task::spawn_blocking(move || store_clone.write_init(&bytes))
            .await
            .map_err(|error| MediaError::Io(std::io::Error::other(error)))??;
    }

    let key_cache: Arc<Mutex<HashMap<String, [u8; 16]>>> = Arc::new(Mutex::new(HashMap::new()));
    let pending: Vec<usize> = completed
        .iter()
        .enumerate()
        .filter_map(|(idx, done)| (!done).then_some(idx))
        .collect();

    let worker_limit = concurrency.max(1);
    let mut in_flight = FuturesUnordered::new();
    let mut next_pending = pending.into_iter();

    for idx in next_pending.by_ref().take(worker_limit) {
        in_flight.push(download_one_segment(SegmentJob {
            client: client.clone(),
            headers: headers.clone(),
            store: store.clone(),
            index: idx,
            source: plan.segments[idx].clone(),
            key_cache: key_cache.clone(),
            bandwidth: bandwidth.clone(),
            cancellation: cancellation.clone(),
            total_downloaded: total_downloaded.clone(),
            progress: progress.clone(),
            estimated_total,
        }));
    }

    let done_count = Arc::new(AtomicU64::new(completed_count as u64));
    while let Some(result) = in_flight.next().await {
        result?;
        let finished = done_count.fetch_add(1, Ordering::Relaxed) + 1;
        let current_bytes = total_downloaded.load(Ordering::Relaxed);
        let refined_total = plan.estimated_bytes.or_else(|| {
            (finished > 0).then(|| (current_bytes / finished) * plan.segments.len() as u64)
        });
        progress(current_bytes, refined_total);

        if let Some(idx) = next_pending.next() {
            if cancellation.is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            in_flight.push(download_one_segment(SegmentJob {
                client: client.clone(),
                headers: headers.clone(),
                store: store.clone(),
                index: idx,
                source: plan.segments[idx].clone(),
                key_cache: key_cache.clone(),
                bandwidth: bandwidth.clone(),
                cancellation: cancellation.clone(),
                total_downloaded: total_downloaded.clone(),
                progress: progress.clone(),
                estimated_total: refined_total,
            }));
        }
    }

    if cancellation.is_cancelled() {
        return Err(MediaError::Cancelled);
    }

    let destination = output_file.to_path_buf();
    let store_concat = store.clone();
    let bytes = tokio::task::spawn_blocking(move || store_concat.concatenate_into(&destination))
        .await
        .map_err(|error| MediaError::Io(std::io::Error::other(error)))??;

    let store_clean = store;
    let _ = tokio::task::spawn_blocking(move || store_clean.cleanup()).await;

    Ok(bytes)
}

struct SegmentJob {
    client: Client,
    headers: HeaderMap,
    store: SegmentStore,
    index: usize,
    source: SegmentSource,
    key_cache: Arc<Mutex<HashMap<String, [u8; 16]>>>,
    bandwidth: BandwidthContext,
    cancellation: CancellationToken,
    total_downloaded: Arc<AtomicU64>,
    progress: Arc<dyn Fn(u64, Option<u64>) + Send + Sync>,
    estimated_total: Option<u64>,
}

async fn download_one_segment(job: SegmentJob) -> Result<(), MediaError> {
    let progress_cb = job.progress.clone();
    let total_downloaded = job.total_downloaded;
    let estimated_total = job.estimated_total;
    let on_chunk = |delta: u64| {
        let current = total_downloaded.fetch_add(delta, Ordering::Relaxed) + delta;
        progress_cb(current, estimated_total);
    };
    let mut payload = fetch_segment_rate_limited(
        &job.client,
        &job.source.url,
        &job.headers,
        job.source.range,
        &job.bandwidth,
        &job.cancellation,
        Some(&on_chunk),
    )
    .await?;

    if let SegmentEncryption::Aes128 { key_url, iv } = job.source.encryption {
        let key = fetch_aes128_key(
            &job.client,
            &job.headers,
            &key_url,
            &job.key_cache,
            &job.cancellation,
        )
        .await?;
        let decryptor = cbc::Decryptor::<aes::Aes128>::new_from_slices(&key, &iv)
            .map_err(|error| MediaError::Decryption(error.to_string()))?;
        let plaintext = decryptor
            .decrypt_padded::<Pkcs7>(&mut payload)
            .map_err(|_| MediaError::Decryption("AES-128 PKCS7 unpadding failed".to_owned()))?;
        let plain_len = plaintext.len();
        payload.truncate(plain_len);
    }

    tokio::task::spawn_blocking(move || job.store.commit_segment(job.index, &payload))
        .await
        .map_err(|error| MediaError::Io(std::io::Error::other(error)))??;
    Ok(())
}

async fn fetch_aes128_key(
    client: &Client,
    headers: &HeaderMap,
    key_url: &Url,
    cache: &Mutex<HashMap<String, [u8; 16]>>,
    cancellation: &CancellationToken,
) -> Result<[u8; 16], MediaError> {
    {
        let guard = cache.lock().await;
        if let Some(key) = guard.get(key_url.as_str()) {
            return Ok(*key);
        }
    }
    let bytes = fetch_bytes(client, key_url, headers, None, cancellation).await?;
    let key: [u8; 16] = bytes.try_into().map_err(|_| {
        MediaError::Decryption(format!("expected 16-byte AES-128 key from {key_url}"))
    })?;
    let mut guard = cache.lock().await;
    guard.insert(key_url.as_str().to_owned(), key);
    Ok(key)
}
