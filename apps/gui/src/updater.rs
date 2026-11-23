//! GitHub release discovery, verified staging, and desktop package replacement.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[cfg(any(unix, test))]
use anyhow::Context as _;
use anyhow::ensure;
use reqwest::{Client, Url};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tempfile::TempPath;
use tokio::io::AsyncWriteExt as _;

const REPOSITORY: &str = "/EmanueleFusco/ruscello/releases/";
const LATEST: &str = "https://api.github.com/repos/EmanueleFusco/ruscello/releases/latest";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_METADATA: usize = 2 * 1024 * 1024;
const MAX_ASSET: u64 = 512 * 1024 * 1024;

#[cfg(any(target_os = "macos", test))]
pub(crate) const STARTUP_MARKER: &str = "RUSCELLO_UPDATE_STARTUP_MARKER";

pub(crate) type Restart = Arc<Mutex<Option<PreparedUpdate>>>;

#[derive(Debug, Clone, Default)]
pub(crate) enum UpdateStatus {
    #[default]
    Idle,
    Checking,
    Current,
    Available(Box<Release>),
    Failed,
}

#[derive(Debug, Clone)]
pub(crate) struct Release {
    pub(crate) tag_name: String,
    pub(crate) body: Option<String>,
    pub(crate) html_url: String,
    pub(crate) date: Option<String>,
    pub(crate) asset: Option<Asset>,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    body: Option<String>,
    html_url: String,
    published_at: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Asset {
    url: Url,
    size: u64,
    sha256: [u8; 32],
    install: Installation,
}

#[derive(Debug, Clone)]
enum Installation {
    #[cfg(target_os = "linux")]
    AppImage(PathBuf),
    #[cfg(windows)]
    Nsis(PathBuf),
    #[cfg(target_os = "macos")]
    AppBundle(PathBuf),
    #[cfg(test)]
    Fixture,
}

impl Installation {
    fn detect() -> Option<Self> {
        #[cfg(target_os = "linux")]
        return image_path().map(Self::AppImage);
        #[cfg(windows)]
        return std::env::current_exe().ok().and_then(|exe| {
            let parent = exe.parent()?;
            parent
                .join("Uninstall.exe")
                .is_file()
                .then(|| Self::Nsis(parent.to_owned()))
        });
        #[cfg(target_os = "macos")]
        return std::env::current_exe().ok().and_then(|exe| {
            let bundle = exe.parent()?.parent()?.parent()?;
            (bundle.extension()? == "app"
                && bundle.join("Contents/Info.plist").is_file()
                && tempfile::tempdir_in(bundle.parent()?).is_ok())
            .then(|| Self::AppBundle(bundle.to_owned()))
        });
    }

    fn extension(&self) -> &'static str {
        match self {
            #[cfg(target_os = "linux")]
            Self::AppImage(_) => "AppImage",
            #[cfg(windows)]
            Self::Nsis(_) => "exe",
            #[cfg(target_os = "macos")]
            Self::AppBundle(_) => "dmg",
            #[cfg(test)]
            Self::Fixture => "fixture",
        }
    }
}

#[cfg(target_os = "linux")]
fn image_path() -> Option<PathBuf> {
    let image = PathBuf::from(std::env::var_os("APPIMAGE")?)
        .canonicalize()
        .ok()?;
    let mount = PathBuf::from(std::env::var_os("APPDIR")?)
        .canonicalize()
        .ok()?;
    (image.is_file() && std::env::current_exe().ok()?.starts_with(mount)).then_some(image)
}

pub(crate) fn relaunch_path() -> std::io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    if let Some(image) = image_path() {
        return Ok(image);
    }
    std::env::current_exe()
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct UpdateProgress {
    pub(crate) downloaded: u64,
    pub(crate) total: u64,
}

impl UpdateProgress {
    pub(crate) fn percent(self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            100.0 * self.downloaded as f32 / self.total as f32
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedUpdate {
    file: Arc<TempPath>,
    install: Installation,
}

fn official(url: &Url, host: &str) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(host)
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
}

fn client(timeout: Duration, fixture: Option<Url>) -> reqwest::Result<Client> {
    Client::builder()
        .user_agent(concat!("Ruscello/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            let url = attempt.url();
            let allowed = official(url, "github.com")
                || official(url, "release-assets.githubusercontent.com")
                || official(url, "objects.githubusercontent.com")
                || fixture
                    .as_ref()
                    .is_some_and(|base| url.origin() == base.origin());
            if allowed && attempt.previous().len() < 5 {
                attempt.follow()
            } else {
                attempt.error("Unexpected release redirect")
            }
        }))
        .build()
}

pub(crate) async fn check_updates() -> UpdateStatus {
    let result = async {
        let installation = tokio::task::spawn_blocking(Installation::detect).await?;
        latest_release(LATEST, installation, None, VERSION).await
    };
    match result.await {
        Ok(Some(release)) => UpdateStatus::Available(Box::new(release)),
        Ok(None) => UpdateStatus::Current,
        Err(error) => {
            tracing::debug!(%error, "Could not check for a release");
            UpdateStatus::Failed
        }
    }
}

async fn latest_release(
    endpoint: &str,
    install: Option<Installation>,
    fixture: Option<Url>,
    current: &str,
) -> anyhow::Result<Option<Release>> {
    let mut response = client(Duration::from_secs(15), fixture.clone())?
        .get(endpoint)
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    response = response.error_for_status()?;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            body.len() + chunk.len() <= MAX_METADATA,
            "Release metadata exceeds size limit"
        );
        body.extend_from_slice(&chunk);
    }
    let release: GitHubRelease = serde_json::from_slice(&body)?;
    let Some(tag) = release.tag_name.strip_prefix('v') else {
        anyhow::bail!("Release tag must start with v");
    };
    let latest = Version::parse(tag)?;
    if release.draft
        || release.prerelease
        || !latest.pre.is_empty()
        || latest.cmp_precedence(&Version::parse(current)?).is_le()
    {
        return Ok(None);
    }
    let html_url = Url::parse(&release.html_url)?;
    ensure!(
        official(&html_url, "github.com")
            && html_url.path() == format!("{REPOSITORY}tag/{}", release.tag_name),
        "Unexpected release page"
    );
    let asset = install.and_then(|install| {
        let name = format!(
            "ruscello-{}-{}-{}.{}",
            release.tag_name,
            std::env::consts::OS,
            std::env::consts::ARCH,
            install.extension()
        );
        let asset = release
            .assets
            .into_iter()
            .find(|asset| asset.name == name)?;
        let url = Url::parse(&asset.browser_download_url).ok()?;
        let expected_path = format!("{REPOSITORY}download/{}/{name}", release.tag_name);
        let allowed = (official(&url, "github.com")
            && url.path() == expected_path
            && url.query().is_none()
            && url.fragment().is_none())
            || fixture
                .as_ref()
                .is_some_and(|base| base.origin() == url.origin());
        if !allowed || !(1..=MAX_ASSET).contains(&asset.size) {
            return None;
        }
        let digest = asset.digest?.strip_prefix("sha256:")?.to_owned();
        let mut sha256 = [0; 32];
        hex::decode_to_slice(digest, &mut sha256).ok()?;
        Some(Asset {
            url,
            size: asset.size,
            sha256,
            install,
        })
    });
    Ok(Some(Release {
        tag_name: release.tag_name,
        body: release.body,
        html_url: release.html_url,
        date: release.published_at,
        asset,
    }))
}

pub(crate) async fn download_and_install(
    asset: Asset,
    mut progress: impl FnMut(UpdateProgress),
) -> anyhow::Result<PreparedUpdate> {
    #[cfg(target_os = "macos")]
    let replacement_directory = match &asset.install {
        Installation::AppBundle(bundle) => bundle.parent().map(std::path::Path::to_path_buf),
        #[cfg(test)]
        Installation::Fixture => None,
    };
    let extension = asset.install.extension();
    let staging = tokio::task::spawn_blocking(move || {
        tempfile::Builder::new()
            .prefix("ruscello-update-")
            .suffix(&format!(".{extension}"))
            .tempfile()
    })
    .await??;
    let (file, path) = staging.into_parts();
    let mut file = tokio::fs::File::from_std(file);
    let mut response = client(Duration::from_secs(20 * 60), None)?
        .get(asset.url)
        .send()
        .await?
        .error_for_status()?;
    let mut hash = Sha256::new();
    let mut state = UpdateProgress {
        downloaded: 0,
        total: asset.size,
    };
    progress(state);
    let mut last = Instant::now();
    while let Some(chunk) = response.chunk().await? {
        state.downloaded += chunk.len() as u64;
        ensure!(
            state.downloaded <= state.total,
            "Release asset exceeds its listed size"
        );
        hash.update(&chunk);
        file.write_all(&chunk).await?;
        if last.elapsed() >= Duration::from_millis(250) {
            progress(state);
            last = Instant::now();
        }
    }
    ensure!(state.downloaded == state.total, "Incomplete release asset");
    ensure!(
        hash.finalize()[..] == asset.sha256,
        "Release SHA-256 did not match"
    );
    file.sync_all().await?;
    drop(file);
    #[cfg(target_os = "macos")]
    if let Some(directory) = replacement_directory {
        tokio::task::spawn_blocking(move || tempfile::tempdir_in(directory).map(drop)).await??;
    }
    progress(state);
    Ok(PreparedUpdate {
        file: Arc::new(path),
        install: asset.install,
    })
}

/// Applies a verified update after the application has drained its engine.
impl PreparedUpdate {
    pub(crate) fn apply(&self) -> anyhow::Result<Option<Relaunch>> {
        match &self.install {
            #[cfg(target_os = "linux")]
            Installation::AppImage(image) => {
                use std::os::unix::fs::PermissionsExt as _;
                let directory = image.parent().context("No AppImage parent directory")?;
                let mut staged = tempfile::NamedTempFile::new_in(directory)?;
                let mut source = std::fs::File::open(&*self.file)?;
                std::io::copy(&mut source, &mut staged)?;
                staged
                    .as_file()
                    .set_permissions(std::fs::Permissions::from_mode(0o755))?;
                staged.as_file().sync_all()?;
                staged.persist(image)?;
                if let Err(error) = std::fs::File::open(directory).and_then(|dir| dir.sync_all()) {
                    tracing::debug!(%error, "Could not sync the AppImage directory");
                }
                Ok(Some(Relaunch::new(image.clone())))
            }
            #[cfg(windows)]
            Installation::Nsis(directory) => {
                use std::os::windows::process::CommandExt as _;
                // NSIS treats all characters after /D= as the install directory.
                let mut destination = std::ffi::OsString::from("/D=");
                destination.push(directory);
                let mut installer = tempfile::Builder::new()
                    .prefix("ruscello-install-")
                    .suffix(".exe")
                    .tempfile()?;
                std::io::copy(
                    &mut std::fs::File::open(&*self.file)?,
                    installer.as_file_mut(),
                )?;
                installer.as_file().sync_all()?;
                let (file, path) = installer.keep()?;
                drop(file);
                let wait_for_desktop = format!("/WAITPID={}", std::process::id());
                let result = std::process::Command::new(&path)
                    .args(["/S", "/UPDATE"])
                    .arg(wait_for_desktop)
                    .raw_arg(destination)
                    .spawn();
                if result.is_err() {
                    let _ = std::fs::remove_file(path);
                }
                result?;
                Ok(None)
            }
            #[cfg(target_os = "macos")]
            Installation::AppBundle(bundle) => {
                let backup = replace_bundle(&self.file, bundle)?;
                Ok(Some(Relaunch {
                    executable: backup.executable(),
                    backup: Some(backup),
                }))
            }
            #[cfg(test)]
            Installation::Fixture => anyhow::bail!("Test fixture cannot be installed"),
        }
    }
}

pub(crate) struct Relaunch {
    pub(crate) executable: PathBuf,
    #[cfg(target_os = "macos")]
    backup: Option<SwappedBundle>,
}

impl Relaunch {
    pub(crate) fn new(executable: PathBuf) -> Self {
        Self {
            executable,
            #[cfg(target_os = "macos")]
            backup: None,
        }
    }

    pub(crate) fn launch(
        self,
        mut command: std::process::Command,
    ) -> std::io::Result<Option<PathBuf>> {
        #[cfg(target_os = "macos")]
        if let Some(backup) = self.backup {
            return backup.launch(&mut command, Duration::from_secs(30));
        }
        command.spawn()?;
        Ok(None)
    }
}

#[cfg(target_os = "macos")]
pub(crate) async fn confirm_startup(marker: PathBuf) {
    if let Err(error) = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker)
        .await
    {
        tracing::warn!(%error, "Could not acknowledge restart after the verified update");
    }
}

#[cfg(target_os = "macos")]
fn replace_bundle(
    dmg: &std::path::Path,
    bundle: &std::path::Path,
) -> anyhow::Result<SwappedBundle> {
    use std::process::Command;
    let parent = bundle.parent().context("No bundle parent directory")?;
    let stage = tempfile::tempdir_in(parent)?;
    let mount = tempfile::tempdir()?;
    ensure!(
        Command::new("/usr/bin/hdiutil")
            .args(["attach", "-readonly", "-nobrowse", "-mountpoint"])
            .arg(mount.path())
            .arg(dmg)
            .output()?
            .status
            .success(),
        "Could not open the verified disk image"
    );
    let next = stage.path().join("Ruscello.app");
    let copied = Command::new("/usr/bin/ditto")
        .arg(mount.path().join("Ruscello.app"))
        .arg(&next)
        .status();
    let detached = Command::new("/usr/bin/hdiutil")
        .arg("detach")
        .arg(mount.path())
        .output();
    ensure!(copied?.success(), "Could not stage the application bundle");
    ensure!(
        detached?.status.success(),
        "Could not unmount the disk image"
    );
    swap_macos_bundle(bundle, &next, stage)
}

#[cfg(any(target_os = "macos", test))]
fn swap_macos_bundle(
    bundle: &std::path::Path,
    next: &std::path::Path,
    stage: tempfile::TempDir,
) -> anyhow::Result<SwappedBundle> {
    ensure!(
        next.join("Contents/MacOS/ruscello").is_file()
            && next.join("Contents/MacOS/ruscello-webview").is_file(),
        "The updated application bundle is incomplete"
    );
    let previous = stage.path().join("previous.app");
    std::fs::rename(bundle, &previous)?;
    if let Err(error) = std::fs::rename(next, bundle) {
        if let Err(rollback) = std::fs::rename(&previous, bundle) {
            let backup = stage.keep();
            tracing::error!(path = %backup.display(), %rollback, "The original application could not be restored");
        }
        return Err(error.into());
    }
    Ok(SwappedBundle {
        bundle: bundle.to_owned(),
        stage,
    })
}

#[cfg(any(target_os = "macos", test))]
struct SwappedBundle {
    bundle: PathBuf,
    stage: tempfile::TempDir,
}

#[cfg(any(target_os = "macos", test))]
impl SwappedBundle {
    fn executable(&self) -> PathBuf {
        self.bundle.join("Contents/MacOS/ruscello")
    }

    fn launch(
        self,
        command: &mut std::process::Command,
        timeout: Duration,
    ) -> std::io::Result<Option<PathBuf>> {
        let marker = self.stage.path().join("started");
        match command.env(STARTUP_MARKER, &marker).spawn() {
            Ok(mut child) => {
                let until = Instant::now() + timeout;
                loop {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            tracing::warn!(%status, "Updated application exited before startup was confirmed");
                            break;
                        }
                        Ok(None) if marker.is_file() => return Ok(None),
                        Ok(None) if Instant::now() < until => {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        result => {
                            // The replacement may be running transfers. Keep the previous
                            // bundle without trying to stop or replace a live process.
                            let backup = self.stage.keep();
                            tracing::warn!(path = %backup.display(), ?result, "Startup unconfirmed; kept the previous application bundle");
                            return Ok(None);
                        }
                    }
                }
            }
            Err(error) => tracing::warn!(%error, "Could not start the updated application"),
        }
        let executable = self.executable();
        self.rollback()?;
        Ok(Some(executable))
    }

    fn rollback(self) -> std::io::Result<()> {
        let result = std::fs::rename(&self.bundle, self.stage.path().join("failed.app"))
            .and_then(|()| std::fs::rename(self.stage.path().join("previous.app"), &self.bundle));
        if let Err(error) = result {
            let backup = self.stage.keep();
            tracing::error!(path = %backup.display(), %error, "The original application could not be restored");
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) async fn fixture_release(
    server: &ruscello_testkit::TestServer,
    expected: &[u8],
    actual: &[u8],
    tag: &str,
    current: &str,
) -> anyhow::Result<Option<Release>> {
    use ruscello_testkit::Reply;
    let file = actual.to_vec();
    let url = server.route("/update", move |_| {
        Reply::bytes("application/octet-stream", file.clone())
    });
    let metadata = serde_json::json!({
        "tag_name": tag,
        "html_url": format!("https://github.com{REPOSITORY}tag/{tag}"),
        "published_at": "2026-11-14T12:00:00Z",
        "draft": false,
        "prerelease": false,
        "body": "A release note from the local fixture.",
        "assets": [{
            "name": format!("ruscello-{tag}-{}-{}.fixture", std::env::consts::OS, std::env::consts::ARCH),
            "browser_download_url": url,
            "size": expected.len(),
            "digest": format!("sha256:{}", hex::encode(Sha256::digest(expected)))
        }],
    }).to_string();
    let release = server.route("/latest", move |_| {
        Reply::bytes("application/json", metadata.clone().into_bytes())
    });
    latest_release(
        &release,
        Some(Installation::Fixture),
        Some(Url::parse(&release)?),
        current,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruscello_testkit::TestServer;

    #[tokio::test]
    async fn discovers_a_newer_release_and_checks_size_and_digest() -> anyhow::Result<()> {
        let server = TestServer::start().await?;
        let bytes = b"an example desktop asset";
        for tag in ["v1.0.0", "v0.9.0", "v2.0.0-rc.1", "v1.0.0+rebuild"] {
            assert!(
                fixture_release(&server, bytes, bytes, tag, "1.0.0")
                    .await?
                    .is_none()
            );
        }
        let fetch = async |payload: &[u8]| {
            let server = TestServer::start().await?;
            let release = fixture_release(&server, bytes, payload, "v1.1.0", "1.0.0")
                .await?
                .context("Missing fixture release")?;
            let asset = release.asset.context("Missing fixture asset")?;
            let mut states = Vec::new();
            let staged = download_and_install(asset, |progress| states.push(progress)).await?;
            assert_eq!(states.first().map(|p| p.downloaded), Some(0));
            assert_eq!(states.last().map(|p| p.percent()), Some(100.0));
            Ok::<_, anyhow::Error>(std::fs::read(&*staged.file)?)
        };
        assert_eq!(fetch(bytes).await?, bytes);
        assert!(fetch(b"AN EXAMPLE DESKTOP ASSET").await.is_err());
        assert!(fetch(b"short").await.is_err());
        assert!(
            fetch(b"longer than an example desktop asset")
                .await
                .is_err()
        );
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn replaces_appimage_without_modifying_the_verified_staging_file() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let image = root.path().join("Ruscello.AppImage");
        std::fs::write(&image, b"previous")?;
        let source = tempfile::NamedTempFile::new_in(root.path())?.into_temp_path();
        std::fs::write(&source, b"new")?;
        let state = PreparedUpdate {
            file: Arc::new(source),
            install: Installation::AppImage(image.clone()),
        };
        assert_eq!(state.apply()?.map(|r| r.executable), Some(image.clone()));
        assert_eq!(std::fs::read(image)?, b"new");
        assert_eq!(std::fs::read(&*state.file)?, b"new");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn replaces_both_macos_binaries_and_rejects_an_incomplete_bundle() -> anyhow::Result<()> {
        use std::{fs, process::Command};

        let root = tempfile::tempdir()?;
        let app = root.path().join("Renamed Ruscello.app");
        let desktop = "Contents/MacOS/ruscello";
        let helper = "Contents/MacOS/ruscello-webview";
        fs::create_dir_all(app.join("Contents/MacOS"))?;
        fs::write(app.join(desktop), b"old desktop")?;
        fs::write(app.join(helper), b"old helper")?;

        let folder = root.path().join("source");
        let bundle = folder.join("Ruscello.app");
        fs::create_dir_all(bundle.join("Contents/MacOS"))?;
        fs::write(bundle.join(desktop), b"new desktop")?;
        assert!(swap_macos_bundle(&app, &bundle, tempfile::tempdir_in(root.path())?).is_err());
        assert_eq!(fs::read(app.join(desktop))?, b"old desktop");
        assert_eq!(fs::read(app.join(helper))?, b"old helper");

        fs::write(bundle.join(helper), b"new helper")?;
        let image = root.path().join("complete.dmg");
        let result = Command::new("/usr/bin/hdiutil")
            .args(["create", "-fs", "HFS+", "-format", "UDZO", "-srcfolder"])
            .arg(&folder)
            .arg(&image)
            .output()?;
        ensure!(
            result.status.success(),
            "Could not create fixture disk image ({}): {}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        );
        let swap = replace_bundle(&image, &app)?;
        let binary = swap.executable();
        assert_eq!(binary, app.join(desktop));
        assert_eq!(fs::read(binary)?, b"new desktop");
        assert_eq!(fs::read(app.join(helper))?, b"new helper");
        Ok(())
    }

    fn macos_swap_fixture() -> anyhow::Result<(tempfile::TempDir, SwappedBundle)> {
        let root = tempfile::tempdir()?;
        let app = root.path().join("Installed.app");
        let stage = tempfile::tempdir_in(root.path())?;
        let next = stage.path().join("New.app");
        for (path, bytes) in [(&app, b"old"), (&next, b"new")] {
            std::fs::create_dir_all(path.join("Contents/MacOS"))?;
            std::fs::write(path.join("Contents/MacOS/ruscello"), bytes)?;
            std::fs::write(path.join("Contents/MacOS/ruscello-webview"), bytes)?;
        }
        Ok((root, swap_macos_bundle(&app, &next, stage)?))
    }

    #[test]
    fn macos_swap_restores_both_binaries_on_spawn_failure_or_early_exit() -> anyhow::Result<()> {
        for early_exit in [false, true] {
            let (_root, swap) = macos_swap_fixture()?;
            let executable = swap.executable();
            let helper = executable.with_file_name("ruscello-webview");
            let stage = swap.stage.path().to_owned();
            assert_eq!(std::fs::read(&executable)?, b"new");
            let mut command = if early_exit {
                let mut command = std::process::Command::new(std::env::current_exe()?);
                command.arg("--list").stdout(std::process::Stdio::null());
                command
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
                }
                std::process::Command::new(&executable)
            };
            assert_eq!(
                swap.launch(&mut command, Duration::from_secs(5))?,
                Some(executable.clone())
            );
            assert_eq!(std::fs::read(executable)?, b"old");
            assert_eq!(std::fs::read(helper)?, b"old");
            assert!(!stage.exists());
        }
        Ok(())
    }

    #[test]
    fn macos_swap_cleans_up_only_after_startup_confirmation() -> anyhow::Result<()> {
        for confirm in [true, false] {
            let (root, swap) = macos_swap_fixture()?;
            let executable = swap.executable();
            let stage = swap.stage.path().to_owned();
            let release = root.path().join("release");
            let mut command = std::process::Command::new(std::env::current_exe()?);
            command
                .args(["--ignored", "--exact", "updater::tests::startup_fixture"])
                .env("RUSCELLO_TEST_RELEASE_PROCESS", &release)
                .stdout(std::process::Stdio::null());
            if confirm {
                command.env("RUSCELLO_TEST_CONFIRM_STARTUP", "1");
            }
            let timeout = if confirm {
                Duration::from_secs(5)
            } else {
                Duration::ZERO
            };
            assert!(swap.launch(&mut command, timeout)?.is_none());
            assert_eq!(!stage.exists(), confirm);
            if !confirm {
                assert_eq!(
                    std::fs::read(stage.join("previous.app/Contents/MacOS/ruscello"))?,
                    b"old"
                );
            }
            assert_eq!(std::fs::read(executable)?, b"new");
            std::fs::write(&release, b"")?;
            let deadline = Instant::now() + Duration::from_secs(5);
            while release.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(25));
            }
            assert!(!release.exists());
        }
        Ok(())
    }

    #[test]
    #[ignore = "child process fixture for the macOS startup confirmation tests"]
    fn startup_fixture() -> anyhow::Result<()> {
        let release = PathBuf::from(
            std::env::var_os("RUSCELLO_TEST_RELEASE_PROCESS").context("No release signal")?,
        );
        if std::env::var_os("RUSCELLO_TEST_CONFIRM_STARTUP").is_some() {
            let marker = std::env::var_os(STARTUP_MARKER).context("No startup marker")?;
            std::fs::File::options()
                .write(true)
                .create_new(true)
                .open(marker)?;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while !release.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        std::fs::remove_file(release)?;
        Ok(())
    }
}
