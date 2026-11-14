use std::{
    net::IpAddr,
    num::{NonZeroU16, NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
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
    Hosters,
    Advanced,
    About,
}

impl Page {
    pub(crate) const ALL: [Self; 9] = [
        Self::General,
        Self::Downloads,
        Self::Speed,
        Self::Torrent,
        Self::Categories,
        Self::Integrations,
        Self::Hosters,
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
            Self::Hosters => "hoster-accounts",
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
    Category(ruscello_core::CategoryId),
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum TorrentOption {
    Dht,
    Pex,
    Upnp,
    Lsd,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum TorrentField {
    Port,
    DownloadLimit,
    UploadLimit,
    Ratio,
    Minutes,
    MaxPeers,
    Interface,
    Blocklist,
}

#[derive(Debug, Default)]
pub(crate) struct CategoryDraft {
    pub(crate) name: String,
    pub(crate) rule: String,
    pub(crate) category: Option<ruscello_core::CategoryId>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum CategoryEdit {
    Name,
    Rule,
}

pub(crate) async fn pick_folder() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .pick_folder()
        .await
        .map(|folder| folder.path().to_path_buf())
}

pub(crate) async fn lan_address() -> Option<IpAddr> {
    ruscello_api::discover_lan_ip_async().await
}

#[derive(Clone)]
pub(crate) struct LanPassword(pub(crate) String);

#[derive(Debug)]
pub(crate) struct HosterDraft {
    pub(crate) host: String,
    pub(crate) username: String,
    pub(crate) password: LanPassword,
    pub(crate) busy: bool,
    pub(crate) result: Option<&'static str>,
}

impl Default for HosterDraft {
    fn default() -> Self {
        Self {
            host: "pixeldrain.com".into(),
            username: String::new(),
            password: LanPassword(String::new()),
            busy: false,
            result: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SidecarDraft {
    pub(crate) ytdlp_custom_path: String,
    pub(crate) ffmpeg_custom_path: String,
    pub(crate) status: Option<ruscello_engine::SidecarsStatus>,
    pub(crate) busy: Option<ruscello_engine::SidecarKind>,
    pub(crate) message: Option<&'static str>,
}

impl SidecarDraft {
    pub(crate) fn from_settings(settings: &ruscello_engine::SidecarSettings) -> Self {
        Self {
            ytdlp_custom_path: settings
                .ytdlp_custom_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            ffmpeg_custom_path: settings
                .ffmpeg_custom_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            ..Self::default()
        }
    }

    pub(crate) fn path_for(&self, kind: ruscello_engine::SidecarKind) -> &str {
        match kind {
            ruscello_engine::SidecarKind::YtDlp => &self.ytdlp_custom_path,
            ruscello_engine::SidecarKind::Ffmpeg => &self.ffmpeg_custom_path,
        }
    }

    pub(crate) fn set_path(&mut self, kind: ruscello_engine::SidecarKind, value: String) {
        match kind {
            ruscello_engine::SidecarKind::YtDlp => self.ytdlp_custom_path = value,
            ruscello_engine::SidecarKind::Ffmpeg => self.ffmpeg_custom_path = value,
        }
    }

    pub(crate) fn parse_custom_path(raw: &str) -> Option<PathBuf> {
        let trimmed = raw.trim();
        (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
    }
}

impl std::fmt::Debug for LanPassword {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LanPassword([redacted])")
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LanPasswordOutcome {
    pub(crate) keyring_saved: bool,
    pub(crate) hash: Option<String>,
}

const LAN_HASH_FILE: &str = "lan-auth.hash";

pub(crate) async fn load_lan_password_hash(data_dir: Option<PathBuf>) -> Option<String> {
    if let Some(dir) = data_dir.as_ref() {
        let path = dir.join(LAN_HASH_FILE);
        if let Ok(raw) = tokio::fs::read_to_string(&path).await {
            let trimmed = raw.trim().to_owned();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    let from_keyring = tokio::task::spawn_blocking(|| {
        keyring::Entry::new("Ruscello LAN Web UI", "remote-control")
            .and_then(|entry| entry.get_password())
            .ok()
            .filter(|password| !password.is_empty())
    })
    .await
    .ok()
    .flatten()?;
    let hash = ruscello_api::hash_password_async(from_keyring).await.ok()?;
    if let Some(dir) = data_dir.as_ref() {
        let _ = write_lan_hash_file(dir, &hash);
    }
    Some(hash)
}

pub(crate) async fn save_lan_password(
    data_dir: Option<PathBuf>,
    password: LanPassword,
) -> LanPasswordOutcome {
    let hash = ruscello_api::hash_password_async(password.0.clone())
        .await
        .ok();
    if let (Some(dir), Some(digest)) = (data_dir.as_ref(), hash.as_ref())
        && let Err(error) = write_lan_hash_file(dir, digest)
    {
        tracing::debug!(%error, "Could not persist LAN verifier hash");
    }
    let keyring_saved = tokio::task::spawn_blocking(move || {
        keyring::Entry::new("Ruscello LAN Web UI", "remote-control")
            .and_then(|entry| entry.set_password(&password.0))
            .is_ok()
    })
    .await
    .unwrap_or(false);
    LanPasswordOutcome {
        keyring_saved,
        hash,
    }
}

fn write_lan_hash_file(dir: &Path, hash: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    std::fs::create_dir_all(dir)?;
    let target = dir.join(LAN_HASH_FILE);
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(hash.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(target)?;
    Ok(())
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

pub(crate) async fn open_latest_log() -> bool {
    tokio::task::spawn_blocking(|| {
        let result = (|| -> anyhow::Result<()> {
            let paths = ruscello_platform::paths::AppPaths::discover()?;
            let latest = std::fs::read_dir(paths.logs().join("ruscello"))?
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_type().is_ok_and(|kind| kind.is_file())
                        && entry
                            .path()
                            .extension()
                            .is_some_and(|extension| extension == "log")
                })
                .max_by_key(std::fs::DirEntry::file_name)
                .ok_or_else(|| anyhow::anyhow!("No desktop log file found"))?;
            open::that_detached(latest.path())?;
            Ok(())
        })();
        result.is_ok()
    })
    .await
    .unwrap_or(false)
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
