use std::{num::NonZeroUsize, sync::Arc};

use ruscello_http::{
    DownloadOptions, ExpiredLinkResolver, HttpError, ResolvedLink, ResolvedTransfer, TransferLink,
    Url,
};
use ruscello_resolver::{Resolution, ResolveError, Resolver};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub struct ResolvedDownload {
    link: TransferLink,
    cap: Option<NonZeroUsize>,
    filename: Option<String>,
    refresh: Arc<Refresh>,
}

impl ResolvedDownload {
    pub fn apply(self, options: &mut DownloadOptions) {
        options.resolved = Some(self.link);
        if let Some(cap) = self.cap {
            options.connections = options.connections.min(cap);
        }
        options.filename_hint = options.filename_hint.take().or(self.filename);
        options.expired_link_resolver = Some(self.refresh);
    }
}

pub async fn resolve_download(
    resolver: &Resolver,
    source: &Url,
    cancellation: &CancellationToken,
) -> Result<Option<ResolvedDownload>, ResolveError> {
    prepare_download(resolver, source, cancellation, false).await
}

pub(super) async fn prepare_download(
    resolver: &Resolver,
    source: &Url,
    cancellation: &CancellationToken,
    hoster: bool,
) -> Result<Option<ResolvedDownload>, ResolveError> {
    let Some(resolution) = resolver
        .resolve_with_xfs(source, cancellation, hoster)
        .await?
    else {
        return Ok(None);
    };
    let cap = resolution.limits.connections;
    let filename = resolution.filename.clone();
    Ok(Some(ResolvedDownload {
        link: transfer_link(resolution),
        cap,
        filename,
        refresh: Arc::new(Refresh {
            resolver: resolver.clone(),
            source: source.clone(),
            cancellation: cancellation.clone(),
            hoster,
        }),
    }))
}

struct Refresh {
    resolver: Resolver,
    source: Url,
    cancellation: CancellationToken,
    hoster: bool,
}

impl std::fmt::Debug for Refresh {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Refresh").finish_non_exhaustive()
    }
}

impl ExpiredLinkResolver for Refresh {
    fn resolve<'a>(&'a self, expired: &'a Url) -> ResolvedLink<'a> {
        Box::pin(async move { self.resolve_transfer(expired).await.map(|link| link.url) })
    }

    fn resolve_transfer<'a>(&'a self, _expired: &'a Url) -> ResolvedTransfer<'a> {
        Box::pin(async move {
            let refreshed = self
                .resolver
                .resolve_with_xfs(&self.source, &self.cancellation, self.hoster)
                .await
                .map_err(|error| match error {
                    ResolveError::Cancelled => HttpError::Cancelled,
                    ResolveError::VerificationRequired => HttpError::VerificationRequired,
                    ResolveError::Status(status) => HttpError::Status(status),
                    ResolveError::Request(error) => HttpError::Request(error),
                    _ => HttpError::ResolutionFailed,
                })?;
            refreshed
                .map(transfer_link)
                .ok_or(HttpError::ResolutionFailed)
        })
    }
}

fn transfer_link(resolution: Resolution) -> TransferLink {
    TransferLink {
        url: resolution.url,
        headers: resolution.headers,
        expires: resolution.expires,
        max_connections: resolution.limits.connections,
    }
}
