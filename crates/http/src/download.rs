use std::{
    collections::BTreeMap,
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
    time::Duration,
};

use bytes::Bytes;
use reqwest::{
    Client, Response, StatusCode, Url,
    header::{self, HeaderValue},
};
use tokio::{
    sync::mpsc,
    task::{AbortHandle, JoinSet},
    time::{self, Instant},
};
use tokio_util::sync::CancellationToken;

use crate::{
    ConnectionController, DEFAULT_MIN_SEGMENT_SIZE, FileWriter, HttpError, Probe, SegmentAllocator,
    Work, WorkId, client, probe,
    probe::{content_range, require_identity},
    writer::TransferKey,
};

const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(2);
const CHECKPOINT_BYTES: u64 = 8 * 1024 * 1024;
const RATE_INTERVAL: Duration = Duration::from_millis(250);
const MAX_CHUNK_BYTES: usize = 128 * 1024;
const MAX_RESTARTS: usize = 3;

/// Settings for one HTTP download.
#[derive(Debug, Clone)]
pub struct DownloadOptions {
    pub directory: PathBuf,
    pub connections: NonZeroUsize,
    pub min_segment_size: NonZeroU64,
    pub cancellation: CancellationToken,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            directory: PathBuf::from("."),
            connections: NonZeroUsize::new(8).unwrap_or(NonZeroUsize::MIN),
            min_segment_size: NonZeroU64::new(DEFAULT_MIN_SEGMENT_SIZE).unwrap_or(NonZeroU64::MIN),
            cancellation: CancellationToken::new(),
        }
    }
}

/// Shares HTTP connections across downloads.
#[derive(Debug, Clone)]
pub struct HttpDownloader {
    client: Client,
}

impl HttpDownloader {
    pub fn new() -> Result<Self, HttpError> {
        Ok(Self { client: client()? })
    }

    /// Downloads a file and returns its published path.
    pub async fn download(
        &self,
        url: &Url,
        options: DownloadOptions,
    ) -> Result<PathBuf, HttpError> {
        for _ in 0..=MAX_RESTARTS {
            let data = tokio::select! {
                () = options.cancellation.cancelled() => return Err(HttpError::Cancelled),
                result = probe(&self.client, url) => result?,
            };
            let size = data.size.ok_or(HttpError::NonResumable)?;
            let source = if size == 0 {
                None
            } else {
                Some(RangeSource::new(&self.client, &data, size).ok_or(HttpError::NonResumable)?)
            };
            let key = TransferKey::new(url, &data);
            let (mut writer, completed, restarted) =
                FileWriter::open_download(&options.directory, &data.filename, key).await?;
            if restarted {
                tracing::warn!("Saved download could not be resumed. Restarting from zero");
            }
            let result = if let Some(source) = source {
                let cap = options
                    .connections
                    .min(NonZeroUsize::new(32).unwrap_or(NonZeroUsize::MIN));
                let allocator =
                    SegmentAllocator::restore(size, cap, options.min_segment_size, &completed);
                if allocator.is_complete() {
                    source.check_complete(&options.cancellation).await
                } else {
                    Segmented::new(source, allocator, cap)
                        .run(&mut writer, &options.cancellation)
                        .await
                }
            } else {
                Ok(())
            };
            match result {
                Ok(()) => return Ok(writer.finish().await?),
                Err(HttpError::RepresentationChanged) => {
                    tracing::warn!("Remote representation changed. Restarting from zero");
                    writer.invalidate_checkpoint().await?;
                    writer.close().await;
                }
                Err(error) => {
                    if matches!(error, HttpError::InvalidResponse(_)) {
                        writer.invalidate_checkpoint().await?;
                    } else if !matches!(error, HttpError::Storage(_)) {
                        writer.checkpoint().await?;
                    }
                    writer.close().await;
                    return Err(error);
                }
            }
        }
        Err(HttpError::RepresentationChanged)
    }
}

#[derive(Clone)]
struct RangeSource {
    client: Client,
    url: Url,
    size: u64,
    validator: HeaderValue,
    has_etag: bool,
}

impl RangeSource {
    fn new(client: &Client, probe: &Probe, size: u64) -> Option<Self> {
        if !probe.supports_ranges {
            return None;
        }
        Some(Self {
            client: client.clone(),
            url: probe.final_url.clone(),
            size,
            validator: probe.validators.if_range()?.clone(),
            has_etag: probe.validators.etag.is_some(),
        })
    }

    async fn check_complete(&self, cancellation: &CancellationToken) -> Result<(), HttpError> {
        tokio::select! {
            () = cancellation.cancelled() => Err(HttpError::Cancelled),
            result = self.response(0, 1) => result.map(|_| ()),
        }
    }

    async fn response(&self, start: u64, end: u64) -> Result<Response, HttpError> {
        let response = self
            .client
            .get(self.url.clone())
            .header(header::ACCEPT_ENCODING, "identity")
            .header(header::RANGE, format!("bytes={start}-{}", end - 1))
            .header(header::IF_RANGE, self.validator.clone())
            .send()
            .await?;
        match response.status() {
            StatusCode::PARTIAL_CONTENT => {}
            StatusCode::OK | StatusCode::RANGE_NOT_SATISFIABLE => {
                return Err(HttpError::RepresentationChanged);
            }
            status => return Err(HttpError::Status(status)),
        }
        let headers = response.headers();
        require_identity(headers)?;
        let (span, total) = content_range(headers)?;
        let actual_validator = headers.get(if self.has_etag {
            header::ETAG
        } else {
            header::LAST_MODIFIED
        });
        if response.url() != &self.url
            || total != Some(self.size)
            || actual_validator.is_some_and(|value| value != self.validator)
            || self.has_etag && actual_validator.is_none()
        {
            return Err(HttpError::RepresentationChanged);
        }
        if span != (start..end)
            || response
                .content_length()
                .is_some_and(|length| length != end - start)
        {
            return Err(HttpError::InvalidResponse(
                "response range does not match the assignment",
            ));
        }
        Ok(response)
    }

    async fn stream(&self, work: &Work, events: &mpsc::Sender<Event>) -> Result<(), HttpError> {
        let mut response = self.response(work.range.start, work.range.end).await?;
        let mut remaining = work.range.end - work.range.start;
        while let Some(bytes) = response.chunk().await? {
            if bytes.len() as u64 > remaining {
                return Err(HttpError::InvalidResponse(
                    "body exceeds the requested range",
                ));
            }
            remaining -= bytes.len() as u64;
            if remaining == 0 && response.chunk().await?.is_some() {
                return Err(HttpError::InvalidResponse(
                    "body exceeds the requested range",
                ));
            }
            let mut offset = 0;
            while offset < bytes.len() {
                let end = (offset + MAX_CHUNK_BYTES).min(bytes.len());
                events
                    .send(Event::Data(work.id, bytes.slice(offset..end)))
                    .await
                    .map_err(|_| HttpError::WorkerStopped)?;
                offset = end;
            }
            if remaining == 0 {
                return Ok(());
            }
        }
        if remaining > 0 {
            return Err(HttpError::Truncated);
        }
        Ok(())
    }
}

enum Event {
    Data(WorkId, Bytes),
    End(WorkId, Result<(), HttpError>),
}

struct Active {
    requested_end: u64,
    task: AbortHandle,
}

struct Segmented {
    source: RangeSource,
    allocator: SegmentAllocator,
    connections: ConnectionController,
    tasks: JoinSet<()>,
    active: BTreeMap<WorkId, Active>,
    events: mpsc::Sender<Event>,
    incoming: mpsc::Receiver<Event>,
}

impl Segmented {
    fn new(source: RangeSource, allocator: SegmentAllocator, cap: NonZeroUsize) -> Self {
        let (events, incoming) = mpsc::channel(cap.get() * 2);
        Self {
            source,
            allocator,
            connections: ConnectionController::new(cap),
            tasks: JoinSet::new(),
            active: BTreeMap::new(),
            events,
            incoming,
        }
    }

    fn schedule(&mut self) {
        while self.active.len() < self.connections.limit() {
            let Some(work) = self.allocator.assign() else {
                break;
            };
            let source = self.source.clone();
            let events = self.events.clone();
            let (id, requested_end) = (work.id, work.range.end);
            let task = self.tasks.spawn(async move {
                let result = source.stream(&work, &events).await;
                let _ = events.send(Event::End(work.id, result)).await;
            });
            self.active.insert(
                id,
                Active {
                    requested_end,
                    task,
                },
            );
        }
    }

    async fn run(
        mut self,
        writer: &mut FileWriter,
        cancellation: &CancellationToken,
    ) -> Result<(), HttpError> {
        let start = Instant::now();
        let mut sample_time = start;
        let mut sample_bytes = self.allocator.completed();
        let mut persisted = sample_bytes;
        let mut rate_tick = time::interval_at(start + RATE_INTERVAL, RATE_INTERVAL);
        rate_tick.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
        let mut checkpoint_tick =
            time::interval_at(start + CHECKPOINT_INTERVAL, CHECKPOINT_INTERVAL);
        checkpoint_tick.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
        loop {
            self.schedule();
            if self.active.is_empty() {
                return if self.allocator.is_complete() {
                    Ok(())
                } else {
                    Err(HttpError::WorkerStopped)
                };
            }
            tokio::select! {
                () = cancellation.cancelled() => return Err(HttpError::Cancelled),
                event = self.incoming.recv() => match event.ok_or(HttpError::WorkerStopped)? {
                    Event::Data(id, bytes) => {
                        self.accept(writer, id, bytes).await?;
                        if self.allocator.completed() - persisted >= CHECKPOINT_BYTES {
                            writer.checkpoint().await?;
                            persisted = self.allocator.completed();
                        }
                    }
                    Event::End(id, result) => {
                        if self.active.remove(&id).is_some() {
                            self.allocator.release(id)?;
                            result?;
                        }
                    }
                },
                task = self.tasks.join_next(), if !self.tasks.is_empty() => {
                    if let Some(Err(error)) = task
                        && !error.is_cancelled()
                    {
                        return Err(HttpError::WorkerStopped);
                    }
                }
                _ = rate_tick.tick() => {
                    let now = Instant::now();
                    let completed = self.allocator.completed();
                    self.connections.observe(completed - sample_bytes, now - sample_time);
                    sample_time = now;
                    sample_bytes = completed;
                }
                _ = checkpoint_tick.tick() => {
                    if self.allocator.completed() > persisted {
                        writer.checkpoint().await?;
                        persisted = self.allocator.completed();
                    }
                }
            }
        }
    }

    async fn accept(
        &mut self,
        writer: &mut FileWriter,
        id: WorkId,
        bytes: Bytes,
    ) -> Result<(), HttpError> {
        let Some(range) = self.allocator.remaining(id) else {
            return Ok(());
        };
        let length = (range.end - range.start).min(bytes.len() as u64);
        if length > 0 {
            writer
                .write_at(range.start, bytes.slice(..length as usize))
                .await?;
            self.allocator.advance(id, length)?;
        }
        if range.start + length == range.end
            && self
                .active
                .get(&id)
                .is_some_and(|active| range.end < active.requested_end)
            && let Some(active) = self.active.remove(&id)
        {
            active.task.abort();
            self.allocator.release(id)?;
        }
        Ok(())
    }
}
