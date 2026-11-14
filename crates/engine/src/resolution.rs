use std::{num::NonZeroUsize, sync::Arc};

use ruscello_http::{
    DownloadOptions, ExpiredLinkResolver, HttpError, ResolvedLink, ResolvedTransfer, TransferLink,
    Url,
};
use ruscello_resolver::{BrowserLocale, Resolution, ResolveError, Resolver};
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

/// Configures plugin resolution and renewal for the standalone CLI and queued transfers.
pub async fn prepare_http_download(
    resolver: &Resolver,
    source: &Url,
    options: &mut DownloadOptions,
    hoster: bool,
    interactive: Option<BrowserLocale>,
) -> Result<(), ResolveError> {
    let cancellation = options.cancellation.clone();
    let resolved = match prepare_download(resolver, source, &cancellation, hoster).await {
        Err(ResolveError::VerificationRequired) if interactive.is_some() => {
            resolver
                .verify(source, interactive.unwrap_or_default(), &cancellation)
                .await?;
            prepare_download(resolver, source, &cancellation, hoster).await?
        }
        result => result?,
    };
    if let Some(resolved) = resolved {
        resolved.apply(options);
    }
    options.expired_link_resolver = Some(Arc::new(Refresh {
        resolver: resolver.clone(),
        source: source.clone(),
        cancellation,
        hoster,
        interactive,
    }));
    Ok(())
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
            interactive: None,
        }),
    }))
}

struct Refresh {
    resolver: Resolver,
    source: Url,
    cancellation: CancellationToken,
    hoster: bool,
    interactive: Option<BrowserLocale>,
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
            let mut refreshed = self
                .resolver
                .recover_link(&self.source, &self.cancellation, self.hoster, true)
                .await;
            if matches!(refreshed, Err(ResolveError::VerificationRequired))
                && let Some(locale) = self.interactive
            {
                self.resolver
                    .verify(&self.source, locale, &self.cancellation)
                    .await
                    .map_err(http_error)?;
                refreshed = self
                    .resolver
                    .recover_link(&self.source, &self.cancellation, self.hoster, false)
                    .await;
            }
            refreshed
                .map_err(http_error)?
                .map(transfer_link)
                .ok_or(HttpError::ResolutionFailed)
        })
    }
}

fn http_error(error: ResolveError) -> HttpError {
    match error {
        ResolveError::Cancelled => HttpError::Cancelled,
        ResolveError::VerificationRequired => HttpError::VerificationRequired,
        ResolveError::Status(status) => HttpError::Status(status),
        ResolveError::Request(error) => HttpError::Request(error),
        _ => HttpError::ResolutionFailed,
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
