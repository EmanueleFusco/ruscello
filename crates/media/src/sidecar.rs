use std::{
    fs::{self, File},
    io::{self, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use reqwest::{Client, Url, header};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use tokio::{process::Command, time};
use tokio_util::sync::CancellationToken;

use crate::{
    MediaError, SidecarBinaryStatus, SidecarEndpoints, SidecarKind, SidecarSettings,
    SidecarsStatus, ffmpeg, storage::sync_directory,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct SidecarState {
    ytdlp_version: Option<String>,
    ytdlp_checked_at_unix: Option<u64>,
    ffmpeg_version: Option<String>,
    ffmpeg_checked_at_unix: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

/// Manages optional `yt-dlp` and `ffmpeg` binaries with explicit user consent and SHA-256 verification.
#[derive(Debug, Clone)]
pub struct SidecarManager {
    data_dir: PathBuf,
    endpoints: SidecarEndpoints,
}

impl SidecarManager {
    /// Creates a manager storing managed binaries and metadata under `data_dir`.
    pub fn new(data_dir: impl Into<PathBuf>, endpoints: SidecarEndpoints) -> Self {
        Self {
            data_dir: data_dir.into(),
            endpoints,
        }
    }

    pub fn endpoints(&self) -> &SidecarEndpoints {
        &self.endpoints
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.data_dir.join("sidecars").join("bin")
    }

    fn state_path(&self) -> PathBuf {
        self.data_dir.join("sidecars").join("state.json")
    }

    fn tmp_dir(&self) -> PathBuf {
        self.data_dir.join("sidecars").join("tmp")
    }

    /// Expected executable name for the host OS.
    pub fn binary_filename(kind: SidecarKind) -> &'static str {
        match kind {
            SidecarKind::YtDlp => {
                if cfg!(windows) {
                    "yt-dlp.exe"
                } else {
                    "yt-dlp"
                }
            }
            SidecarKind::Ffmpeg => {
                if cfg!(windows) {
                    "ffmpeg.exe"
                } else {
                    "ffmpeg"
                }
            }
        }
    }

    /// Returns the resolved binary path if configured via custom path or installed as a managed sidecar.
    pub fn resolve_binary(
        &self,
        kind: SidecarKind,
        settings: &SidecarSettings,
    ) -> Option<(PathBuf, bool)> {
        let custom = match kind {
            SidecarKind::YtDlp => settings.ytdlp_custom_path.as_ref(),
            SidecarKind::Ffmpeg => settings.ffmpeg_custom_path.as_ref(),
        };
        if let Some(path) = custom
            && !path.as_os_str().is_empty()
            && path.is_file()
        {
            return Some((path.clone(), false));
        }
        let managed = self.bin_dir().join(Self::binary_filename(kind));
        if managed.is_file() {
            return Some((managed, true));
        }
        None
    }

    /// Reads current sidecar installation status from disk and probes binary versions if needed.
    pub async fn status(&self, settings: &SidecarSettings) -> SidecarsStatus {
        let state = self.load_state();
        let ytdlp = self
            .binary_status(
                SidecarKind::YtDlp,
                settings.ytdlp_consent,
                settings.ytdlp_custom_path.clone(),
                state.ytdlp_version,
            )
            .await;
        let ffmpeg = self
            .binary_status(
                SidecarKind::Ffmpeg,
                settings.ffmpeg_consent,
                settings.ffmpeg_custom_path.clone(),
                state.ffmpeg_version,
            )
            .await;
        SidecarsStatus {
            ytdlp,
            ffmpeg,
            ytdlp_auto_update: settings.ytdlp_auto_update,
        }
    }

    async fn binary_status(
        &self,
        kind: SidecarKind,
        consent_granted: bool,
        custom_path: Option<PathBuf>,
        known_version: Option<String>,
    ) -> SidecarBinaryStatus {
        let mut status = SidecarBinaryStatus {
            installed: false,
            managed: false,
            path: None,
            version: None,
            consent_granted,
            custom_path: custom_path.clone(),
            latest_known_version: None,
        };
        let settings = match kind {
            SidecarKind::YtDlp => SidecarSettings {
                ytdlp_custom_path: custom_path,
                ..Default::default()
            },
            SidecarKind::Ffmpeg => SidecarSettings {
                ffmpeg_custom_path: custom_path,
                ..Default::default()
            },
        };
        if let Some((path, managed)) = self.resolve_binary(kind, &settings) {
            status.installed = true;
            status.managed = managed;
            status.path = Some(path.clone());
            status.version = if managed {
                if let Some(v) = known_version {
                    Some(v)
                } else {
                    probe_sidecar_version(kind, &path).await
                }
            } else {
                probe_sidecar_version(kind, &path).await
            };
        }
        status
    }

    /// Downloads, verifies (SHA-256), and installs `kind` ONLY when explicit consent is granted.
    pub async fn install_or_update(
        &self,
        kind: SidecarKind,
        settings: &SidecarSettings,
        client: &Client,
        cancellation: &CancellationToken,
    ) -> Result<SidecarBinaryStatus, MediaError> {
        let consented = match kind {
            SidecarKind::YtDlp => settings.ytdlp_consent,
            SidecarKind::Ffmpeg => settings.ffmpeg_consent,
        };
        if !consented {
            return Err(MediaError::ConsentRequired(kind));
        }

        let release_url = match kind {
            SidecarKind::YtDlp => &self.endpoints.ytdlp_release_url,
            SidecarKind::Ffmpeg => &self.endpoints.ffmpeg_release_url,
        };
        let release = fetch_release(client, release_url, cancellation).await?;
        let asset = select_platform_asset(kind, &release.assets).ok_or_else(|| {
            MediaError::ArchiveExtract(format!("no {kind} release asset matches current platform"))
        })?;
        let checksum_asset =
            select_checksum_asset(&asset.name, &release.assets).ok_or_else(|| {
                MediaError::ArchiveExtract(format!(
                    "SHA-256 checksum asset missing for {}",
                    asset.name
                ))
            })?;

        let checksum_url = Url::parse(&checksum_asset.browser_download_url)
            .map_err(|_| MediaError::InvalidUrl(checksum_asset.browser_download_url.clone()))?;
        let checksum_bytes = crate::net::fetch_bytes(
            client,
            &checksum_url,
            &header::HeaderMap::new(),
            None,
            cancellation,
        )
        .await?;
        let checksum_text = String::from_utf8_lossy(&checksum_bytes);
        let expected_sha256 =
            parse_sha256_for_asset(&checksum_text, &asset.name).ok_or_else(|| {
                MediaError::ArchiveExtract(format!(
                    "no SHA-256 entry found for {} in {}",
                    asset.name, checksum_asset.name
                ))
            })?;

        let asset_url = Url::parse(&asset.browser_download_url)
            .map_err(|_| MediaError::InvalidUrl(asset.browser_download_url.clone()))?;

        fs::create_dir_all(self.tmp_dir())?;
        fs::create_dir_all(self.bin_dir())?;

        let downloaded_tmp = download_and_verify(
            client,
            &asset_url,
            &self.tmp_dir(),
            &expected_sha256,
            cancellation,
        )
        .await?;

        let target_path = self.bin_dir().join(Self::binary_filename(kind));
        let asset_name = asset.name.clone();
        let target_clone = target_path.clone();
        let bin_dir = self.bin_dir();

        tokio::task::spawn_blocking(move || {
            install_payload(
                downloaded_tmp.path(),
                &asset_name,
                Self::binary_filename(kind),
                &target_clone,
                &bin_dir,
            )
        })
        .await
        .map_err(|error| MediaError::Io(io::Error::other(error)))??;

        let mut state = self.load_state();
        let now = now_unix();
        match kind {
            SidecarKind::YtDlp => {
                state.ytdlp_version = Some(release.tag_name.clone());
                state.ytdlp_checked_at_unix = Some(now);
            }
            SidecarKind::Ffmpeg => {
                state.ffmpeg_version = Some(release.tag_name.clone());
                state.ffmpeg_checked_at_unix = Some(now);
            }
        }
        self.save_state(&state)?;

        Ok(SidecarBinaryStatus {
            installed: true,
            managed: true,
            path: Some(target_path),
            version: Some(release.tag_name.clone()),
            consent_granted: consented,
            custom_path: match kind {
                SidecarKind::YtDlp => settings.ytdlp_custom_path.clone(),
                SidecarKind::Ffmpeg => settings.ffmpeg_custom_path.clone(),
            },
            latest_known_version: Some(release.tag_name),
        })
    }

    /// Periodically checks and updates the managed `yt-dlp` binary when auto-update & consent are enabled.
    pub async fn maybe_auto_update_ytdlp(
        &self,
        settings: &SidecarSettings,
        client: &Client,
        cancellation: &CancellationToken,
    ) -> Result<Option<String>, MediaError> {
        if !settings.ytdlp_consent || !settings.ytdlp_auto_update {
            return Ok(None);
        }
        if settings
            .ytdlp_custom_path
            .as_ref()
            .is_some_and(|p| p.is_file())
        {
            return Ok(None);
        }
        if !self
            .bin_dir()
            .join(Self::binary_filename(SidecarKind::YtDlp))
            .is_file()
        {
            return Ok(None);
        }
        let mut state = self.load_state();
        let now = now_unix();
        if let Some(last) = state.ytdlp_checked_at_unix
            && now.saturating_sub(last) < self.endpoints.auto_update_interval_secs
        {
            return Ok(None);
        }
        let release =
            fetch_release(client, &self.endpoints.ytdlp_release_url, cancellation).await?;
        if state.ytdlp_version.as_deref() == Some(release.tag_name.as_str()) {
            state.ytdlp_checked_at_unix = Some(now);
            self.save_state(&state)?;
            return Ok(None);
        }
        let updated = self
            .install_or_update(SidecarKind::YtDlp, settings, client, cancellation)
            .await?;
        Ok(updated.version)
    }

    fn load_state(&self) -> SidecarState {
        fs::read(self.state_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save_state(&self, state: &SidecarState) -> Result<(), MediaError> {
        let dir = self.data_dir.join("sidecars");
        fs::create_dir_all(&dir)?;
        let json = serde_json::to_vec_pretty(state)
            .map_err(|error| MediaError::Io(io::Error::other(error)))?;
        crate::storage::write_bytes_atomic(&self.state_path(), &dir, &json)?;
        Ok(())
    }
}

async fn probe_sidecar_version(kind: SidecarKind, binary: &Path) -> Option<String> {
    match kind {
        SidecarKind::Ffmpeg => ffmpeg::probe_version(binary).await,
        SidecarKind::YtDlp => {
            let output = time::timeout(
                Duration::from_secs(5),
                Command::new(binary)
                    .arg("--version")
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output(),
            )
            .await
            .ok()?
            .ok()?;
            output.status.success().then(|| {
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_owned()
            })
        }
    }
}

async fn fetch_release(
    client: &Client,
    release_url: &str,
    cancellation: &CancellationToken,
) -> Result<GithubRelease, MediaError> {
    let url =
        Url::parse(release_url).map_err(|_| MediaError::InvalidUrl(release_url.to_owned()))?;
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::ACCEPT,
        header::HeaderValue::from_static("application/vnd.github+json"),
    );
    let bytes = crate::net::fetch_bytes(client, &url, &headers, None, cancellation).await?;
    serde_json::from_slice(&bytes).map_err(|error| {
        MediaError::ArchiveExtract(format!("invalid release metadata JSON: {error}"))
    })
}

async fn download_and_verify(
    client: &Client,
    url: &Url,
    tmp_dir: &Path,
    expected_sha256: &str,
    cancellation: &CancellationToken,
) -> Result<NamedTempFile, MediaError> {
    let sent = tokio::select! {
        () = cancellation.cancelled() => return Err(MediaError::Cancelled),
        res = client.get(url.clone()).send() => res?,
    };
    if !sent.status().is_success() {
        return Err(MediaError::HttpStatus(sent.status().as_u16()));
    }
    let mut response = sent;
    let mut temp = NamedTempFile::new_in(tmp_dir)?;
    let mut hasher = Sha256::new();
    loop {
        let chunk = tokio::select! {
            () = cancellation.cancelled() => return Err(MediaError::Cancelled),
            next = response.chunk() => next,
        }?;
        let Some(bytes) = chunk else {
            break;
        };
        hasher.update(&bytes);
        temp.write_all(&bytes)?;
    }
    temp.as_file().sync_all()?;
    let actual = hex::encode(hasher.finalize());
    let expected_lower = expected_sha256.trim().to_ascii_lowercase();
    if actual != expected_lower {
        return Err(MediaError::ChecksumMismatch {
            expected: expected_lower,
            actual,
        });
    }
    Ok(temp)
}

fn select_platform_asset(kind: SidecarKind, assets: &[GithubAsset]) -> Option<&GithubAsset> {
    match kind {
        SidecarKind::YtDlp => {
            let candidates: &[&str] = if cfg!(target_os = "windows") {
                &["yt-dlp.exe"]
            } else if cfg!(target_os = "macos") {
                &["yt-dlp_macos", "yt-dlp"]
            } else if cfg!(target_arch = "aarch64") {
                &["yt-dlp_linux_aarch64", "yt-dlp_linux", "yt-dlp"]
            } else {
                &["yt-dlp_linux", "yt-dlp"]
            };
            candidates
                .iter()
                .find_map(|name| assets.iter().find(|asset| asset.name == *name))
        }
        SidecarKind::Ffmpeg => {
            let preferred = |asset: &GithubAsset, os_tag: &str, ext: &str| {
                let lower = asset.name.to_ascii_lowercase();
                lower.contains(os_tag) && lower.contains("gpl") && lower.ends_with(ext)
            };
            let fallback = |asset: &GithubAsset, os_tag: &str| {
                let lower = asset.name.to_ascii_lowercase();
                lower.contains(os_tag) && !lower.ends_with(".sha256") && !lower.contains("shared")
            };
            if cfg!(target_os = "windows") {
                assets
                    .iter()
                    .find(|a| preferred(a, "win64", ".zip"))
                    .or_else(|| assets.iter().find(|a| fallback(a, "win64")))
                    .or_else(|| assets.iter().find(|a| a.name == "ffmpeg.exe"))
            } else if cfg!(target_os = "macos") {
                assets
                    .iter()
                    .find(|a| {
                        let lower = a.name.to_ascii_lowercase();
                        (lower.contains("macos") || lower.contains("osx"))
                            && !lower.ends_with(".sha256")
                    })
                    .or_else(|| assets.iter().find(|a| a.name == "ffmpeg"))
            } else if cfg!(target_arch = "aarch64") {
                assets
                    .iter()
                    .find(|a| preferred(a, "linuxarm64", ".tar.xz"))
                    .or_else(|| assets.iter().find(|a| fallback(a, "linuxarm64")))
                    .or_else(|| assets.iter().find(|a| a.name == "ffmpeg"))
            } else {
                assets
                    .iter()
                    .find(|a| preferred(a, "linux64", ".tar.xz"))
                    .or_else(|| assets.iter().find(|a| fallback(a, "linux64")))
                    .or_else(|| assets.iter().find(|a| a.name == "ffmpeg"))
            }
        }
    }
}

fn select_checksum_asset<'a>(
    asset_name: &str,
    assets: &'a [GithubAsset],
) -> Option<&'a GithubAsset> {
    let specific = format!("{asset_name}.sha256");
    assets
        .iter()
        .find(|a| a.name == specific)
        .or_else(|| assets.iter().find(|a| a.name == "SHA2-256SUMS"))
        .or_else(|| assets.iter().find(|a| a.name == "checksums.sha256"))
        .or_else(|| {
            assets.iter().find(|a| {
                let lower = a.name.to_ascii_lowercase();
                lower.contains("sha256") || lower.contains("sha2-256")
            })
        })
}

fn parse_sha256_for_asset(sums_text: &str, asset_name: &str) -> Option<String> {
    let lines: Vec<&str> = sums_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    if lines.len() == 1
        && let Some(only) = lines.first()
        && only.len() == 64
        && only.chars().all(|ch| ch.is_ascii_hexdigit())
    {
        return Some(only.to_ascii_lowercase());
    }
    for line in lines {
        let mut parts = line.split_whitespace();
        let Some(hash) = parts.next() else {
            continue;
        };
        if hash.len() != 64 || !hash.chars().all(|ch| ch.is_ascii_hexdigit()) {
            continue;
        }
        let Some(raw_name) = parts.next() else {
            continue;
        };
        let normalized = raw_name.trim_start_matches('*').trim_start_matches("./");
        let base = Path::new(normalized)
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or(normalized);
        if normalized == asset_name || base == asset_name {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

fn install_payload(
    downloaded_file: &Path,
    asset_name: &str,
    target_bin_name: &str,
    target_path: &Path,
    bin_dir: &Path,
) -> Result<(), MediaError> {
    fs::create_dir_all(bin_dir)?;
    let lower = asset_name.to_ascii_lowercase();
    let mut temp = NamedTempFile::new_in(bin_dir)?;

    if lower.ends_with(".zip") {
        extract_from_zip(downloaded_file, target_bin_name, temp.as_file_mut())?;
    } else if lower.ends_with(".tar.xz") {
        let file = File::open(downloaded_file)?;
        let mut reader = BufReader::new(file);
        let mut decompressed = Vec::new();
        lzma_rs::xz_decompress(&mut reader, &mut decompressed).map_err(|error| {
            MediaError::ArchiveExtract(format!("xz decompression failed: {error}"))
        })?;
        extract_from_tar(
            io::Cursor::new(decompressed),
            target_bin_name,
            temp.as_file_mut(),
        )?;
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        let file = File::open(downloaded_file)?;
        let gz = flate2::read::GzDecoder::new(file);
        extract_from_tar(gz, target_bin_name, temp.as_file_mut())?;
    } else {
        let mut input = File::open(downloaded_file)?;
        io::copy(&mut input, temp.as_file_mut())?;
    }

    temp.as_file().sync_all()?;
    set_executable(temp.as_file())?;
    temp.persist(target_path).map_err(|error| error.error)?;
    sync_directory(bin_dir)?;
    Ok(())
}

fn extract_from_zip(
    zip_path: &Path,
    target_bin_name: &str,
    out: &mut File,
) -> Result<(), MediaError> {
    let file = File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| MediaError::ArchiveExtract(format!("invalid zip archive: {error}")))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|error| MediaError::ArchiveExtract(format!("zip entry error: {error}")))?;
        if entry.is_dir() {
            continue;
        }
        let Some(name) = entry
            .enclosed_name()
            .and_then(|p| p.file_name().and_then(|f| f.to_str()).map(str::to_owned))
        else {
            continue;
        };
        if name == target_bin_name {
            io::copy(&mut entry, out)?;
            return Ok(());
        }
    }
    Err(MediaError::ArchiveExtract(format!(
        "{target_bin_name} not found inside zip archive"
    )))
}

fn extract_from_tar<R: Read>(
    reader: R,
    target_bin_name: &str,
    out: &mut File,
) -> Result<(), MediaError> {
    let mut archive = tar::Archive::new(reader);
    let entries = archive
        .entries()
        .map_err(|error| MediaError::ArchiveExtract(format!("invalid tar archive: {error}")))?;
    for entry in entries {
        let mut entry = entry
            .map_err(|error| MediaError::ArchiveExtract(format!("tar entry error: {error}")))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|error| MediaError::ArchiveExtract(format!("tar path error: {error}")))?;
        let matches = path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name == target_bin_name);
        if matches {
            io::copy(&mut entry, out)?;
            return Ok(());
        }
    }
    Err(MediaError::ArchiveExtract(format!(
        "{target_bin_name} not found inside tar archive"
    )))
}

#[cfg(unix)]
fn set_executable(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut perms = file.metadata()?.permissions();
    perms.set_mode(0o755);
    file.set_permissions(perms)
}

#[cfg(not(unix))]
fn set_executable(_file: &File) -> io::Result<()> {
    Ok(())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sha256sums_lines_and_single_hashes() {
        let expected = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let sums = format!(
            "# comment\n{expected}  yt-dlp_linux\nffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff *other.zip\n"
        );
        assert_eq!(
            parse_sha256_for_asset(&sums, "yt-dlp_linux").as_deref(),
            Some(expected)
        );
        assert_eq!(
            parse_sha256_for_asset(expected, "ffmpeg.tar.xz").as_deref(),
            Some(expected)
        );
        assert_eq!(
            parse_sha256_for_asset("badhash  yt-dlp_linux", "yt-dlp_linux"),
            None
        );
    }
}
