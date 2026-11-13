use std::{
    net::{IpAddr, UdpSocket},
    num::{NonZeroU16, NonZeroU64, NonZeroUsize},
    path::PathBuf,
    time::Duration,
};

use ruscello_engine::Limits;
use serde::Deserialize;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    #[default]
    General,
    Downloads,
    Speed,
    Torrent,
    Categories,
    Integrations,
    Advanced,
    About,
}

impl Page {
    pub(crate) const ALL: [Self; 8] = [
        Self::General,
        Self::Downloads,
        Self::Speed,
        Self::Torrent,
        Self::Categories,
        Self::Integrations,
        Self::Advanced,
        Self::About,
    ];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Downloads => "downloads",
            Self::Speed => "speed",
            Self::Torrent => "torrent-input",
            Self::Categories => "categories",
            Self::Integrations => "integrations",
            Self::Advanced => "advanced",
            Self::About => "about",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Number {
    Concurrent,
    PerHost,
    PerDownload,
    Bandwidth,
    Alternate,
    Port,
}

impl Number {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Concurrent => "limit-concurrent",
            Self::PerHost => "limit-host",
            Self::PerDownload => "limit-download",
            Self::Bandwidth => "limit-bandwidth",
            Self::Alternate => "limit-alternate",
            Self::Port => "lan-port",
        }
    }

    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Default)]
pub(crate) struct Draft {
    pub(crate) fields: [String; 6],
}

impl Draft {
    pub(crate) fn new(limits: Limits, alternate: NonZeroU64, port: NonZeroU16) -> Self {
        Self {
            fields: [
                limits.concurrent_downloads.to_string(),
                limits.connections_per_host.to_string(),
                limits.connections_per_download.to_string(),
                limits
                    .bandwidth
                    .map_or_else(String::new, |rate| rate.to_string()),
                alternate.to_string(),
                port.to_string(),
            ],
        }
    }

    pub(crate) fn parse(&self, mut limits: Limits) -> Option<(Limits, NonZeroU64, NonZeroU16)> {
        let value = |field: Number| self.fields[field.index()].trim();
        limits.concurrent_downloads = value(Number::Concurrent).parse::<NonZeroUsize>().ok()?;
        limits.connections_per_host = value(Number::PerHost).parse::<NonZeroUsize>().ok()?;
        limits.connections_per_download =
            value(Number::PerDownload).parse::<NonZeroUsize>().ok()?;
        let bandwidth = value(Number::Bandwidth);
        limits.bandwidth = if bandwidth.is_empty() {
            None
        } else {
            Some(bandwidth.parse().ok()?)
        };
        Some((
            limits,
            value(Number::Alternate).parse().ok()?,
            value(Number::Port).parse().ok()?,
        ))
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Folder {
    Default,
    Temporary,
}

pub(crate) async fn pick_folder() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .pick_folder()
        .await
        .map(|folder| folder.path().to_path_buf())
}

pub(crate) async fn lan_address() -> Option<IpAddr> {
    tokio::task::spawn_blocking(|| {
        let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
        socket.connect("192.0.2.1:9").ok()?;
        Some(socket.local_addr().ok()?.ip())
    })
    .await
    .ok()
    .flatten()
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Release {
    pub(crate) tag_name: String,
    pub(crate) body: Option<String>,
    pub(crate) html_url: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) enum UpdateStatus {
    #[default]
    Idle,
    Checking,
    Current,
    Available(Release),
    Failed,
}

pub(crate) async fn check_updates() -> UpdateStatus {
    match latest_release().await {
        Ok(Some(release)) => UpdateStatus::Available(release),
        Ok(None) => UpdateStatus::Current,
        Err(error) => {
            tracing::debug!(%error, "Could not check for a release");
            UpdateStatus::Failed
        }
    }
}

async fn latest_release() -> anyhow::Result<Option<Release>> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("Ruscello/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(12))
        .build()?;
    let response = client
        .get("https://api.github.com/repos/EmanueleFusco/ruscello/releases/latest")
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let release = response.error_for_status()?.json::<Release>().await?;
    let available = semver::Version::parse(release.tag_name.trim_start_matches('v'))?;
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))?;
    let url = ruscello_engine::Url::parse(&release.html_url)?;
    anyhow::ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("github.com")
            && url.path().starts_with("/EmanueleFusco/ruscello/releases/"),
        "Unexpected release URL"
    );
    Ok((available > current).then_some(release))
}

pub(crate) fn diagnostics(renderer: crate::preferences::Renderer) -> String {
    format!(
        "Ruscello {}\nPlatform: {} ({})\nRenderer: {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        renderer.backend()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_limits_and_lan_port_without_changing_unedited_settings() -> anyhow::Result<()> {
        let limits = Limits {
            retries: 4,
            ..Limits::default()
        };
        let mut draft = Draft::new(limits, NonZeroU64::MIN, NonZeroU16::MIN);
        assert_eq!(
            draft.parse(limits),
            Some((limits, NonZeroU64::MIN, NonZeroU16::MIN))
        );
        draft.fields[Number::Port.index()] = "65536".into();
        assert!(draft.parse(limits).is_none());
        draft.fields[Number::Port.index()] = "9666".into();
        draft.fields[Number::Bandwidth.index()] = "1048576".into();
        let (edited, _, _) = draft
            .parse(limits)
            .ok_or_else(|| anyhow::anyhow!("valid draft rejected"))?;
        assert_eq!(edited.retries, 4);
        assert_eq!(edited.bandwidth.map(NonZeroU64::get), Some(1_048_576));
        Ok(())
    }
}
