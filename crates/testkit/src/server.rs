use std::{
    collections::HashMap,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    ops::Range,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::stream;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use tokio::{net::TcpListener, task::JoinHandle, time::sleep};

use crate::{FileSpec, Payload, Representation};

type Files = Arc<Mutex<Vec<Arc<Resource>>>>;
type Routes = Arc<Mutex<HashMap<String, Arc<dyn Fn(Request) -> Reply + Send + Sync>>>>;
const FILENAME_ENCODING: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'.')
    .remove(b'-')
    .remove(b'_')
    .remove(b'~');

/// An HTTP server on an ephemeral IPv4 loopback port. Dropping it closes the listener.
pub struct TestServer {
    address: SocketAddr,
    files: Files,
    routes: Routes,
    task: JoinHandle<io::Result<()>>,
}

impl std::fmt::Debug for TestServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TestServer")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct Request {
    pub method: axum::http::Method,
    pub headers: HeaderMap,
    pub uri: axum::http::Uri,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub enum Reply {
    Response(StatusCode, HeaderMap, String),
    Bytes(StatusCode, HeaderMap, Vec<u8>),
    File(TestFile),
}

impl Reply {
    pub fn html(contents: impl Into<String>) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
        Self::Response(StatusCode::OK, headers, contents.into())
    }

    pub fn bytes(content_type: &'static str, body: impl Into<Vec<u8>>) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
        Self::Bytes(StatusCode::OK, headers, body.into())
    }

    pub fn status(status: StatusCode) -> Self {
        Self::Response(status, HeaderMap::new(), String::new())
    }
}

impl TestServer {
    pub async fn start() -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let files = Files::default();
        let routes = Routes::default();
        let router = Router::new()
            .route("/file/{id}", get(serve_file))
            .route("/file/{id}/", get(serve_file))
            .route("/file/{id}/{name}", get(serve_named_file))
            .route("/redirect/{id}/{hops}", get(redirect))
            .with_state(files.clone())
            .fallback({
                let routes = routes.clone();
                move |ConnectInfo(peer): ConnectInfo<SocketAddr>,
                      request: axum::extract::Request| {
                    respond_custom(routes.clone(), peer, request)
                }
            });
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
        });
        Ok(Self {
            address,
            files,
            routes,
            task,
        })
    }

    pub fn route(
        &self,
        path: &str,
        handler: impl Fn(Request) -> Reply + Send + Sync + 'static,
    ) -> String {
        lock(&self.routes).insert(path.to_owned(), Arc::new(handler));
        format!("http://{}{path}", self.address)
    }

    /// Registers a new route and returns its URL and observation handle.
    pub fn file(&self, spec: FileSpec) -> TestFile {
        let mut files = lock(&self.files);
        let id = files.len();
        let path = match spec.redirect_hops {
            0 => file_path(id, &spec.filename),
            hops => format!("/redirect/{id}/{hops}"),
        };
        let resource = Arc::new(Resource::new(spec));
        files.push(resource.clone());
        TestFile {
            url: format!("http://{}{path}", self.address),
            resource,
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Request and response information recorded at the final file route.
#[derive(Clone, Debug)]
pub struct RequestRecord {
    pub range: Option<String>,
    pub if_range: Option<String>,
    pub status: StatusCode,
}

/// A registered fixture and its observations.
#[derive(Clone, Debug)]
pub struct TestFile {
    url: String,
    resource: Arc<Resource>,
}

impl TestFile {
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn requests(&self) -> Vec<RequestRecord> {
        lock(&self.resource.requests).clone()
    }

    /// Replaces the representation for subsequent requests.
    pub fn replace(&self, value: Representation) {
        *lock(&self.resource.override_representation) = Some(value);
    }

    pub fn payload(&self) -> Payload {
        self.resource.representation().payload()
    }
}

#[derive(Debug)]
struct Resource {
    spec: FileSpec,
    override_representation: Mutex<Option<Representation>>,
    emitted: AtomicU64,
    request_count: AtomicU64,
    drop_count: AtomicU64,
    connections: Mutex<HashMap<IpAddr, usize>>,
    requests: Mutex<Vec<RequestRecord>>,
}

impl Resource {
    fn new(spec: FileSpec) -> Self {
        Self {
            spec,
            override_representation: Mutex::default(),
            emitted: AtomicU64::default(),
            request_count: AtomicU64::default(),
            drop_count: AtomicU64::default(),
            connections: Mutex::default(),
            requests: Mutex::default(),
        }
    }

    fn representation(&self) -> Representation {
        if let Some(value) = lock(&self.override_representation).as_ref() {
            return value.clone();
        }
        if let Some((threshold, replacement)) = &self.spec.replacement
            && self.emitted.load(Ordering::Relaxed) >= *threshold
        {
            return replacement.clone();
        }
        self.spec.representation.clone()
    }

    fn acquire(self: &Arc<Self>, ip: IpAddr) -> Result<Option<Lease>, StatusCode> {
        let Some((limit, status)) = self.spec.max_connections else {
            return Ok(None);
        };
        let mut active = lock(&self.connections);
        if active.get(&ip).copied().unwrap_or(0) >= limit {
            return Err(status);
        }
        *active.entry(ip).or_default() += 1;
        Ok(Some(Lease {
            resource: self.clone(),
            ip,
        }))
    }

    fn stream_end(&self, span: &Range<u64>) -> u64 {
        let Some((after, limit)) = self.spec.drop_after else {
            return span.end;
        };
        if after < span.end - span.start
            && limit.is_none_or(|limit| self.drop_count.fetch_add(1, Ordering::Relaxed) < limit)
        {
            span.start + after
        } else {
            span.end
        }
    }
}

struct Lease {
    resource: Arc<Resource>,
    ip: IpAddr,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut active = lock(&self.resource.connections);
        if let Some(count) = active.get_mut(&self.ip) {
            *count -= 1;
            if *count == 0 {
                active.remove(&self.ip);
            }
        }
    }
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn file_path(id: usize, name: &str) -> String {
    if name.is_empty() {
        format!("/file/{id}/")
    } else {
        let name = utf8_percent_encode(name, FILENAME_ENCODING);
        format!("/file/{id}/{name}")
    }
}

async fn redirect(State(files): State<Files>, Path((id, hops)): Path<(usize, u8)>) -> Response {
    let Some(resource) = lock(&files).get(id).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let target = if hops > 1 {
        format!("/redirect/{id}/{}", hops - 1)
    } else {
        file_path(id, &resource.spec.filename)
    };
    let mut response = StatusCode::TEMPORARY_REDIRECT.into_response();
    set_header(&mut response, header::LOCATION, &target);
    response
}

async fn serve_file(
    State(files): State<Files>,
    Path(id): Path<usize>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    respond(files, id, peer.ip(), headers).await
}

async fn serve_named_file(
    State(files): State<Files>,
    Path((id, _)): Path<(usize, String)>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    respond(files, id, peer.ip(), headers).await
}

async fn respond(files: Files, id: usize, ip: IpAddr, headers: HeaderMap) -> Response {
    let resource = lock(&files).get(id).cloned();
    let Some(resource) = resource else {
        return StatusCode::NOT_FOUND.into_response();
    };
    respond_resource(&resource, ip, &headers).await
}

async fn respond_custom(
    routes: Routes,
    peer: SocketAddr,
    request: axum::extract::Request,
) -> Response {
    let handler = lock(&routes).get(request.uri().path()).cloned();
    let Some(handler) = handler else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let (parts, body) = request.into_parts();
    let body = match axum::body::to_bytes(body, 8 * 1024 * 1024).await {
        Ok(body) => body.to_vec(),
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    match handler(Request {
        method: parts.method,
        headers: parts.headers.clone(),
        uri: parts.uri,
        body,
    }) {
        Reply::Response(status, headers, body) => (status, headers, body).into_response(),
        Reply::Bytes(status, headers, body) => (status, headers, body).into_response(),
        Reply::File(file) => respond_resource(&file.resource, peer.ip(), &parts.headers).await,
    }
}

async fn respond_resource(resource: &Arc<Resource>, ip: IpAddr, headers: &HeaderMap) -> Response {
    let value = |name| headers.get(name).and_then(|header| header.to_str().ok());
    let range = value(header::RANGE);
    let if_range = value(header::IF_RANGE);
    let response = file_response(resource, ip, range, if_range).await;
    lock(&resource.requests).push(RequestRecord {
        range: range.map(str::to_owned),
        if_range: if_range.map(str::to_owned),
        status: response.status(),
    });
    response
}

async fn file_response(
    resource: &Arc<Resource>,
    ip: IpAddr,
    range: Option<&str>,
    if_range: Option<&str>,
) -> Response {
    let lease = match resource.acquire(ip) {
        Ok(lease) => lease,
        Err(status) => return status.into_response(),
    };
    if !resource.spec.latency.is_zero() {
        sleep(resource.spec.latency).await;
    }
    let index = resource.request_count.fetch_add(1, Ordering::Relaxed) + 1;
    if let Some((period, status)) = resource.spec.intermittent
        && index.is_multiple_of(period.get())
    {
        return status.into_response();
    }

    let representation = resource.representation();
    let size = representation.payload.len();
    let matches = if_range.is_none_or(|value| {
        (value.starts_with('"') && representation.etag.as_deref() == Some(value))
            || representation.modified.as_deref() == Some(value)
    });
    let requested = range.filter(|_| resource.spec.ranges && matches);
    let (status, span) = match requested {
        Some(value) => match parse_range(value, size) {
            Some(span) => (StatusCode::PARTIAL_CONTENT, span),
            None => {
                let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
                set_header(
                    &mut response,
                    header::CONTENT_RANGE,
                    &format!("bytes */{size}"),
                );
                return response;
            }
        },
        None => (StatusCode::OK, 0..size),
    };

    let length = span.end - span.start;
    let end = resource.stream_end(&span);
    let body = StreamBody {
        offset: span.start,
        end,
        broken: end < span.end,
        payload: representation.payload,
        resource: resource.clone(),
        _lease: lease,
    };
    let mut response = Response::new(Body::from_stream(stream::unfold(body, StreamBody::next)));
    *response.status_mut() = status;
    if !resource.spec.chunked {
        set_header(&mut response, header::CONTENT_LENGTH, &length.to_string());
    }
    if resource.spec.ranges {
        set_header(&mut response, header::ACCEPT_RANGES, "bytes");
    }
    if status == StatusCode::PARTIAL_CONTENT {
        let (start, end) = (span.start, span.end - 1);
        set_header(
            &mut response,
            header::CONTENT_RANGE,
            &format!("bytes {start}-{end}/{size}"),
        );
    }
    for (key, value) in [
        (header::ETAG, representation.etag.as_deref()),
        (header::LAST_MODIFIED, representation.modified.as_deref()),
        (
            header::CONTENT_DISPOSITION,
            resource.spec.disposition.as_deref(),
        ),
        (header::CONTENT_TYPE, resource.spec.content_type.as_deref()),
    ] {
        if let Some(value) = value {
            set_header(&mut response, key, value);
        }
    }
    response
}

fn parse_range(value: &str, size: u64) -> Option<Range<u64>> {
    let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
    if start.is_empty() {
        let count = end.parse::<u64>().ok()?;
        return (count > 0 && size > 0).then_some(size.saturating_sub(count)..size);
    }
    let start = start.parse::<u64>().ok()?;
    let end = if end.is_empty() {
        size
    } else {
        end.parse::<u64>().ok()?.saturating_add(1).min(size)
    };
    (start < end).then_some(start..end)
}

fn set_header(response: &mut Response, name: HeaderName, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        response.headers_mut().insert(name, value);
    }
}

struct StreamBody {
    offset: u64,
    end: u64,
    broken: bool,
    payload: Payload,
    resource: Arc<Resource>,
    _lease: Option<Lease>,
}

impl StreamBody {
    async fn next(mut self) -> Option<(io::Result<Vec<u8>>, Self)> {
        if self.offset == self.end {
            if !self.broken {
                return None;
            }
            self.broken = false;
            // Let the server flush the preceding body frames before breaking the stream.
            sleep(Duration::from_millis(10)).await;
            return Some((
                Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "fixture disconnect",
                )),
                self,
            ));
        }
        let throttle = self.resource.spec.throttle.map(std::num::NonZeroU64::get);
        let chunk = throttle.map_or(64 * 1024, |rate| (rate / 20).clamp(1, 64 * 1024));
        let count = chunk.min(self.end - self.offset);
        if let Some(rate) = throttle {
            sleep(Duration::from_secs_f64(count as f64 / rate as f64)).await;
        }
        let next = self.offset + count;
        let bytes = self.payload.bytes(self.offset..next);
        self.offset = next;
        self.resource.emitted.fetch_add(count, Ordering::Relaxed);
        Some((Ok(bytes), self))
    }
}
