use std::{
    collections::BTreeMap,
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use reqwest::{
    StatusCode, Url,
    header::{self, HeaderMap, HeaderValue},
};
use tokio::{
    sync::{mpsc, watch},
    task::{AbortHandle, JoinSet},
    time::{self, Instant},
};
use tokio_util::sync::CancellationToken;

use crate::{
    BandwidthLimit, ConnectionController, Connectivity, DEFAULT_MIN_SEGMENT_SIZE, ErrorClass,
    ExpectedChecksum, ExpiredLinkResolver, FileWriter, HostConnections, HttpError, Probe,
    RetryPolicy, SegmentAllocator, TransferLink, Work, WorkId, WriterError,
    probe::{content_range, is_page, looks_like_page, require_identity},
    retry::{self, RetryState, status_error},
    transport::{ActiveResponse, Transport},
    writer::{CheckpointJob, Recovery, TransferKey},
};

mod single;

const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(2);
const CHECKPOINT_BYTES: u64 = 64 * 1024 * 1024;
const RATE_INTERVAL: Duration = Duration::from_millis(250);
const RAMP_INTERVAL: Duration = Duration::from_millis(100);
const MAX_CHUNK_BYTES: usize = 128 * 1024;
const MAX_RESTARTS: usize = 3;

/// Synchronous preflight executed on a blocking worker after probing and before opening the writer.
pub trait DownloadPreparation: std::fmt::Debug + Send + Sync {
    fn prepare(&self, metadata: &Probe) -> Result<(), WriterError>;
}

/// Settings for one HTTP download.
#[derive(Debug, Clone)]
pub struct DownloadOptions {
    pub directory: PathBuf,
    pub connections: NonZeroUsize,
    pub min_segment_size: NonZeroU64,
    pub cancellation: CancellationToken,
    pub retry: RetryPolicy,
    pub bandwidth: BandwidthLimit,
    /// An additional per-download rate applied alongside the shared limits.
    pub download_bandwidth: Option<NonZeroU64>,
    pub progress: Option<watch::Sender<DownloadProgress>>,
    pub expired_link_resolver: Option<Arc<dyn ExpiredLinkResolver>>,
    /// The first resolved URL, distinct from the logical source used for checkpoints.
    pub resolved: Option<TransferLink>,
    pub filename_hint: Option<String>,
    pub mirrors: Vec<Url>,
    pub expected_checksum: Option<ExpectedChecksum>,
    pub checkpoint_job: Option<CheckpointJob>,
    /// Optional staging directory on the same volume as the partial file.
    pub completion_directory: Option<PathBuf>,
    pub preparation: Option<Arc<dyn DownloadPreparation>>,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            directory: PathBuf::from("."),
            connections: NonZeroUsize::new(8).unwrap_or(NonZeroUsize::MIN),
            min_segment_size: NonZeroU64::new(DEFAULT_MIN_SEGMENT_SIZE).unwrap_or(NonZeroU64::MIN),
            cancellation: CancellationToken::new(),
            retry: RetryPolicy::default(),
            bandwidth: BandwidthLimit::default(),
            download_bandwidth: None,
            progress: None,
            expired_link_resolver: None,
            resolved: None,
            filename_hint: None,
            mirrors: Vec::new(),
            expected_checksum: None,
            checkpoint_job: None,
            completion_directory: None,
            preparation: None,
        }
    }
}

impl DownloadOptions {
    fn report(&self, progress: DownloadProgress) {
        if let Some(sender) = &self.progress {
            sender.send_if_modified(|current| {
                let changed = *current != progress;
                *current = progress;
                changed
            });
        }
    }

    fn waiting(&self, waiting: bool) {
        if let Some(sender) = &self.progress {
            sender.send_if_modified(|current| {
                let changed = current.waiting_for_network != waiting;
                current.waiting_for_network = waiting;
                changed
            });
        }
    }
}

/// Progress snapshot for a download. An unknown length is represented by `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DownloadProgress {
    pub completed: u64,
    pub total: Option<u64>,
    pub resumable: bool,
    pub restarted: bool,
    pub waiting_for_network: bool,
    pub segments: Option<SegmentProgress>,
}

/// A bounded display map for the segmented transfer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SegmentProgress {
    pub complete: u64,
    pub active: u64,
    pub connections: usize,
}

impl SegmentProgress {
    fn sample(allocator: &SegmentAllocator, connections: usize) -> Self {
        let (complete, active) = allocator.display_map();
        Self {
            complete,
            active,
            connections,
        }
    }
}

/// Shares HTTP connections across downloads.
#[derive(Debug, Clone)]
pub struct HttpDownloader {
    transport: Transport,
    global_bandwidth: BandwidthLimit,
    connectivity: Connectivity,
}

impl HttpDownloader {
    pub fn new() -> Result<Self, HttpError> {
        Self::with_hosts(None)
    }

    pub fn with_host_connections(hosts: HostConnections) -> Result<Self, HttpError> {
        Self::with_hosts(Some(hosts))
    }

    fn with_hosts(hosts: Option<HostConnections>) -> Result<Self, HttpError> {
        Ok(Self {
            transport: Transport::new(hosts)?,
            global_bandwidth: BandwidthLimit::default(),
            connectivity: Connectivity::default(),
        })
    }

    pub fn global_bandwidth(&self) -> BandwidthLimit {
        self.global_bandwidth.clone()
    }

    pub fn connectivity(&self) -> Connectivity {
        self.connectivity.clone()
    }

    /// Downloads a file and returns its published path.
    pub async fn download(
        &self,
        url: &Url,
        options: DownloadOptions,
    ) -> Result<PathBuf, HttpError> {
        if !options.mirrors.is_empty() && options.expected_checksum.is_none() {
            return Err(HttpError::MirrorChecksumRequired);
        }
        let mut current = options
            .resolved
            .clone()
            .unwrap_or_else(|| TransferLink::new(url.clone()));
        let mut state = DownloadState {
            cap: options
                .connections
                .min(NonZeroUsize::new(32).unwrap_or(NonZeroUsize::MIN)),
            retry: RetryState::default(),
            restarted: false,
        };
        let mut representation_changes = 0;
        let mut refreshes = 0;
        let mut resolved = options.resolved.is_some();
        loop {
            if !self.connectivity.is_online() {
                options.waiting(true);
                self.connectivity.wait_online(&options.cancellation).await?;
                options.waiting(false);
            }
            let error = if current
                .expires
                .is_some_and(|expires| expires <= SystemTime::now())
            {
                HttpError::ExpiredRepresentation
            } else {
                match Box::pin(self.attempt(url, &current, &options, &mut state, resolved)).await {
                    Ok(path) => return Ok(path),
                    Err(error) => error,
                }
            };
            if matches!(error, HttpError::RepresentationChanged) {
                if representation_changes == MAX_RESTARTS {
                    return Err(error);
                }
                representation_changes += 1;
                state.restart();
                continue;
            }
            if (error.classification() == ErrorClass::ExpiredLink
                || matches!(error, HttpError::VerificationRequired))
                && let Some(resolver) = &options.expired_link_resolver
                && refreshes < MAX_RESTARTS
            {
                refreshes += 1;
                current = tokio::select! {
                    () = options.cancellation.cancelled() => return Err(HttpError::Cancelled),
                    result = resolver.resolve_transfer(&current.url) => result?,
                };
                resolved = true;
                if let Some(cap) = current.max_connections {
                    state.cap = state.cap.min(cap);
                }
                continue;
            }
            let Some(delay) =
                state
                    .retry
                    .next_delay(&error, &options.retry, self.connectivity.is_online())
            else {
                return Err(error);
            };
            tracing::debug!(?delay, "Retrying HTTP download");
            if error.offline() {
                options.waiting(true);
            }
            retry::wait(delay, &options.cancellation).await?;
            options.waiting(false);
        }
    }

    async fn attempt(
        &self,
        original: &Url,
        current: &TransferLink,
        options: &DownloadOptions,
        state: &mut DownloadState,
        resolved: bool,
    ) -> Result<PathBuf, HttpError> {
        let mut data = tokio::select! {
            () = options.cancellation.cancelled() => return Err(HttpError::Cancelled),
            result = self.transport.probe_with(&current.url, current.headers.clone()) => result?,
        };
        if resolved && !data.attachment && is_page(data.content_type.as_deref()) {
            return Err(HttpError::ExpiredRepresentation);
        }
        if let Some(filename) = &options.filename_hint {
            data.filename.clone_from(filename);
        }
        let headers = if data.final_url.origin() == current.url.origin() {
            current.headers.clone()
        } else {
            HeaderMap::new()
        };
        if let Some(preparation) = options.preparation.clone() {
            let metadata = data.clone();
            tokio::task::spawn_blocking(move || preparation.prepare(&metadata))
                .await
                .map_err(|_| HttpError::WorkerStopped)??;
        }
        if options.cancellation.is_cancelled() {
            return Err(HttpError::Cancelled);
        }
        let limits = TransferLimits {
            global: self.global_bandwidth.clone(),
            local: options.bandwidth.clone(),
            individual: options
                .download_bandwidth
                .map(|rate| BandwidthLimit::new(Some(rate))),
        };
        let source = data.size.and_then(|size| {
            (size > 0)
                .then(|| {
                    RangeSource::new(
                        &self.transport,
                        &data,
                        size,
                        limits.clone(),
                        headers.clone(),
                        resolved,
                    )
                })
                .flatten()
        });
        let key = TransferKey::new(
            original,
            &data,
            options.expected_checksum.as_ref(),
            resolved,
        );
        let recovery = options
            .checkpoint_job
            .clone()
            .map(|job| Recovery::new(job, original));
        let (mut writer, completed, restarted) =
            FileWriter::open_download(&options.directory, &data.filename, key, recovery, resolved)
                .await?;
        if restarted {
            state.restart();
        }
        let mut progress = DownloadProgress {
            total: data.size,
            resumable: source.is_some(),
            restarted: state.restarted,
            ..DownloadProgress::default()
        };
        let result = if let Some(source) = source {
            let allocator = SegmentAllocator::restore(
                source.size,
                state.cap,
                options.min_segment_size,
                &completed,
            );
            progress.completed = allocator.completed();
            progress.segments = Some(SegmentProgress::sample(&allocator, 0));
            options.report(progress);
            if allocator.is_complete() {
                source.check_complete(&options.cancellation).await
            } else {
                let mirrors = self.mirrors(&source, &limits, options).await?;
                let mut segmented = Segmented::new(source, mirrors, allocator, state.cap);
                let result = segmented.run(&mut writer, options, progress).await;
                progress.completed = segmented.allocator.completed();
                progress.segments = Some(SegmentProgress::sample(&segmented.allocator, 0));
                state.retry.record_progress(progress.completed);
                if result.as_ref().is_err_and(|error| error.congested()) {
                    state.cap = NonZeroUsize::new(segmented.connections.congested())
                        .unwrap_or(NonZeroUsize::MIN);
                }
                result
            }
        } else {
            options.report(progress);
            if data.size == Some(0) {
                Ok(())
            } else {
                match single::download(
                    &self.transport,
                    &data,
                    &limits,
                    &mut writer,
                    options,
                    progress,
                    (headers, resolved),
                )
                .await
                {
                    Ok(completed) => {
                        progress.completed = completed;
                        progress.total = Some(completed);
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
        };
        match result {
            Ok(()) => {
                let path = if let Some(checksum) = options.expected_checksum.clone() {
                    match writer
                        .finish_verified(
                            checksum,
                            options.cancellation.clone(),
                            options.completion_directory.clone(),
                        )
                        .await
                    {
                        Err(WriterError::Cancelled) => return Err(HttpError::Cancelled),
                        result => result?,
                    }
                } else {
                    writer
                        .finish_into(options.completion_directory.clone())
                        .await?
                };
                options.report(progress);
                Ok(path)
            }
            Err(error) => {
                if matches!(
                    error,
                    HttpError::RepresentationChanged | HttpError::InvalidResponse(_)
                ) {
                    writer.invalidate_checkpoint().await?;
                } else if !matches!(error, HttpError::Storage(_)) {
                    writer.checkpoint().await?;
                }
                writer.close().await;
                Err(error)
            }
        }
    }
    async fn mirrors(
        &self,
        primary: &RangeSource,
        limits: &TransferLimits,
        options: &DownloadOptions,
    ) -> Result<Vec<RangeSource>, HttpError> {
        let mut sources = Vec::<RangeSource>::new();
        for url in &options.mirrors {
            let metadata = tokio::select! {
                () = options.cancellation.cancelled() => return Err(HttpError::Cancelled),
                result = self.transport.probe(url) => result,
            };
            let Ok(metadata) = metadata else {
                tracing::warn!("Skipping an unavailable mirror");
                continue;
            };
            if metadata.final_url == primary.url
                || sources
                    .iter()
                    .any(|source| source.url == metadata.final_url)
            {
                continue;
            }
            if metadata.size == Some(primary.size)
                && let Some(source) = RangeSource::new(
                    &self.transport,
                    &metadata,
                    primary.size,
                    limits.clone(),
                    HeaderMap::new(),
                    false,
                )
            {
                sources.push(source);
            } else {
                tracing::warn!("Skipping a mirror without matching size or validated byte ranges");
            }
        }
        Ok(sources)
    }
}

struct DownloadState {
    cap: NonZeroUsize,
    retry: RetryState,
    restarted: bool,
}

impl DownloadState {
    fn restart(&mut self) {
        if !self.restarted {
            tracing::warn!("The saved representation could not be resumed. Restarting from zero");
        }
        self.restarted = true;
    }
}

#[derive(Clone)]
struct TransferLimits {
    global: BandwidthLimit,
    local: BandwidthLimit,
    individual: Option<BandwidthLimit>,
}

impl TransferLimits {
    async fn next_chunk(&self, bytes: &Bytes, offset: usize) -> Bytes {
        let requested = (bytes.len() - offset).min(MAX_CHUNK_BYTES);
        let granted = self
            .global
            .acquire(&self.local, self.individual.as_ref(), requested)
            .await;
        bytes.slice(offset..offset + granted)
    }
}

#[derive(Clone)]
struct RangeSource {
    transport: Transport,
    url: Url,
    size: u64,
    validator: HeaderValue,
    has_etag: bool,
    limits: TransferLimits,
    headers: HeaderMap,
    resolved: bool,
}

impl RangeSource {
    fn new(
        transport: &Transport,
        probe: &Probe,
        size: u64,
        limits: TransferLimits,
        headers: HeaderMap,
        resolved: bool,
    ) -> Option<Self> {
        if !probe.supports_ranges {
            return None;
        }
        Some(Self {
            transport: transport.clone(),
            url: probe.final_url.clone(),
            size,
            validator: probe.validators.if_range()?.clone(),
            has_etag: probe.validators.etag.is_some(),
            limits,
            headers,
            resolved,
        })
    }

    async fn check_complete(&self, cancellation: &CancellationToken) -> Result<(), HttpError> {
        tokio::select! {
            () = cancellation.cancelled() => Err(HttpError::Cancelled),
            result = self.response(0, 1) => result.map(|_| ()),
        }
    }

    async fn response(&self, start: u64, end: u64) -> Result<ActiveResponse, HttpError> {
        let mut headers = self.headers.clone();
        let range = HeaderValue::from_str(&format!("bytes={start}-{}", end - 1))
            .map_err(|_| HttpError::InvalidResponse("invalid range request"))?;
        headers.insert(header::RANGE, range);
        headers.insert(header::IF_RANGE, self.validator.clone());
        let response = self
            .transport
            .get(&self.url, headers, false)
            .await
            .map_err(|error| {
                if self.resolved && matches!(error, HttpError::RepresentationChanged) {
                    HttpError::ExpiredRepresentation
                } else {
                    error
                }
            })?;
        if self.resolved && looks_like_page(response.headers()) {
            return Err(HttpError::ExpiredRepresentation);
        }
        match response.status() {
            StatusCode::PARTIAL_CONTENT => {}
            StatusCode::OK | StatusCode::RANGE_NOT_SATISFIABLE => {
                return Err(HttpError::RepresentationChanged);
            }
            _ => return Err(status_error(&response)),
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
                let chunk = self.limits.next_chunk(&bytes, offset).await;
                offset += chunk.len();
                events
                    .send(Event::Data(work.id, chunk))
                    .await
                    .map_err(|_| HttpError::WorkerStopped)?;
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
    source_index: usize,
    task: AbortHandle,
}

struct Segmented {
    source: RangeSource,
    mirrors: Vec<Option<RangeSource>>,
    next_source: usize,
    allocator: SegmentAllocator,
    connections: ConnectionController,
    tasks: JoinSet<()>,
    active: BTreeMap<WorkId, Active>,
    events: mpsc::Sender<Event>,
    incoming: mpsc::Receiver<Event>,
}

impl Segmented {
    fn new(
        source: RangeSource,
        mirrors: Vec<RangeSource>,
        allocator: SegmentAllocator,
        cap: NonZeroUsize,
    ) -> Self {
        let (events, incoming) = mpsc::channel(cap.get() * 2);
        Self {
            source,
            mirrors: mirrors.into_iter().map(Some).collect(),
            next_source: 0,
            allocator,
            connections: ConnectionController::with_initial(
                cap,
                NonZeroUsize::new((cap.get() / 4).clamp(1, 4)).unwrap_or(NonZeroUsize::MIN),
            ),
            tasks: JoinSet::new(),
            active: BTreeMap::new(),
            events,
            incoming,
        }
    }

    fn choose_source(&mut self) -> (usize, RangeSource) {
        loop {
            let index = self.next_source;
            self.next_source = (index + 1) % (self.mirrors.len() + 1);
            if index == 0 {
                return (index, self.source.clone());
            }
            if let Some(Some(mirror)) = self.mirrors.get(index - 1) {
                return (index, mirror.clone());
            }
        }
    }

    fn schedule(&mut self) {
        while self.active.len() < self.connections.limit() {
            let Some(work) = self.allocator.assign() else {
                break;
            };
            let (source_index, source) = self.choose_source();
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
                    source_index,
                    task,
                },
            );
        }
    }

    async fn run(
        &mut self,
        writer: &mut FileWriter,
        options: &DownloadOptions,
        mut progress: DownloadProgress,
    ) -> Result<(), HttpError> {
        let start = Instant::now();
        let mut sample_time = start;
        let mut reported = start;
        let mut sample_bytes = self.allocator.completed();
        let mut persisted = sample_bytes;
        let mut rate_tick = time::interval_at(start + RAMP_INTERVAL, RAMP_INTERVAL);
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
                () = options.cancellation.cancelled() => return Err(HttpError::Cancelled),
                event = self.incoming.recv() => match event.ok_or(HttpError::WorkerStopped)? {
                    Event::Data(id, bytes) => {
                        self.accept(writer, id, bytes).await?;
                        if self.allocator.completed() - persisted >= CHECKPOINT_BYTES {
                            writer.checkpoint().await?;
                            persisted = self.allocator.completed();
                        }
                    }
                    Event::End(id, result) => {
                        if let Some(active) = self.active.remove(&id) {
                            self.allocator.release(id)?;
                            if let Err(error) = result {
                                if active.source_index == 0 {
                                    return Err(error);
                                }
                                if let Some(mirror) = self.mirrors.get_mut(active.source_index - 1) {
                                    *mirror = None;
                                }
                                if error.congested() {
                                    self.connections.congested();
                                }
                                tracing::warn!(class = ?error.classification(), "Disabled a failed mirror");
                            }
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
                    let previous_limit = self.connections.limit();
                    let limit = self.connections.observe(completed - sample_bytes, now - sample_time);
                    if limit != previous_limit {
                        tracing::debug!(limit, "Adjusted HTTP connection budget");
                    }
                    sample_time = now;
                    sample_bytes = completed;
                    if now - reported >= RATE_INTERVAL {
                        progress.completed = completed;
                        progress.segments = Some(SegmentProgress::sample(&self.allocator, self.active.len()));
                        options.report(progress);
                        reported = now;
                    }
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
                .queue_at(range.start, bytes.slice(..length as usize))
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
