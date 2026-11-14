use reqwest::{
    StatusCode,
    header::{self, HeaderMap},
};
use tokio::time::Instant;

use crate::{
    DownloadOptions, DownloadProgress, FileWriter, HttpError, Probe,
    probe::{looks_like_page, require_identity},
    retry::status_error,
    transport::Transport,
};

use super::{RATE_INTERVAL, TransferLimits};

/// Streams one complete representation. A retry always starts at byte zero.
pub(super) async fn download(
    transport: &Transport,
    probe: &Probe,
    limits: &TransferLimits,
    writer: &mut FileWriter,
    options: &DownloadOptions,
    mut progress: DownloadProgress,
    headers: HeaderMap,
) -> Result<u64, HttpError> {
    let transfer = async {
        let mut response = transport
            .get(&probe.final_url, headers, false)
            .await
            .map_err(|error| {
                if options.resolved.is_some() && matches!(error, HttpError::RepresentationChanged) {
                    HttpError::ExpiredRepresentation
                } else {
                    error
                }
            })?;
        if options.resolved.is_some() && looks_like_page(response.headers()) {
            return Err(HttpError::ExpiredRepresentation);
        }
        if response.status() != StatusCode::OK {
            return Err(status_error(&response));
        }
        let headers = response.headers();
        require_identity(headers)?;
        if response.url() != &probe.final_url
            || probe
                .size
                .zip(response.content_length())
                .is_some_and(|(before, after)| before != after)
            || probe
                .validators
                .etag
                .as_ref()
                .zip(headers.get(header::ETAG))
                .is_some_and(|(before, after)| before != after)
            || probe
                .validators
                .last_modified
                .as_ref()
                .zip(headers.get(header::LAST_MODIFIED))
                .is_some_and(|(before, after)| before != after)
        {
            return Err(HttpError::RepresentationChanged);
        }

        let length = probe.size.or(response.content_length());
        progress.total = length;
        let mut notified = Instant::now();
        while let Some(bytes) = response.chunk().await? {
            if length.is_some_and(|length| {
                progress.completed.saturating_add(bytes.len() as u64) > length
            }) {
                return Err(HttpError::InvalidResponse(
                    "body exceeds the declared length",
                ));
            }
            let mut cursor = 0;
            while cursor < bytes.len() {
                let chunk = limits.next_chunk(&bytes, cursor).await;
                let size = chunk.len();
                writer.queue_at(progress.completed, chunk).await?;
                cursor += size;
                progress.completed += size as u64;
                if notified.elapsed() >= RATE_INTERVAL {
                    options.report(progress);
                    notified = Instant::now();
                }
            }
        }
        if length.is_some_and(|length| progress.completed < length) {
            return Err(HttpError::Truncated);
        }
        options.report(progress);
        Ok(progress.completed)
    };
    tokio::select! {
        () = options.cancellation.cancelled() => Err(HttpError::Cancelled),
        result = transfer => result,
    }
}
