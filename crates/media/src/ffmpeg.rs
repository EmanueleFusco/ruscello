use std::{
    fs::{self, File},
    path::Path,
    process::Stdio,
    time::Duration,
};

use tokio::{process::Command, time};
use tokio_util::sync::CancellationToken;

use crate::{MediaError, SidecarKind, storage::sync_directory};

/// Probes the `--version` output of a `ffmpeg` binary.
pub(crate) async fn probe_version(ffmpeg_bin: &Path) -> Option<String> {
    let output = time::timeout(
        Duration::from_secs(5),
        Command::new(ffmpeg_bin)
            .arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let first = text.lines().next()?.trim();
    if let Some(rest) = first.strip_prefix("ffmpeg version ") {
        Some(rest.split_whitespace().next().unwrap_or(first).to_owned())
    } else if !first.is_empty() {
        Some(first.to_owned())
    } else {
        None
    }
}

/// Remuxes separate video and audio elementary/container tracks into `output` using `-c copy`.
pub(crate) async fn remux_av(
    ffmpeg_bin: &Path,
    video_path: &Path,
    audio_path: &Path,
    output: &Path,
    cancellation: &CancellationToken,
) -> Result<(), MediaError> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let ext = output
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("mp4");
    let temp = tempfile::Builder::new()
        .prefix(".ffmpeg-remux-")
        .suffix(&format!(".{ext}"))
        .tempfile_in(parent)?;
    let temp_path = temp.into_temp_path();

    let mut command = Command::new(ffmpeg_bin);
    command
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(video_path)
        .arg("-i")
        .arg(audio_path)
        .arg("-c")
        .arg("copy")
        .arg(&temp_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = command
        .spawn()
        .map_err(|error| MediaError::SidecarProcess {
            kind: SidecarKind::Ffmpeg,
            detail: format!("failed to start ffmpeg: {error}"),
        })?;

    let output_result = tokio::select! {
        () = cancellation.cancelled() => return Err(MediaError::Cancelled),
        done = child.wait_with_output() => done.map_err(|error| MediaError::SidecarProcess {
            kind: SidecarKind::Ffmpeg,
            detail: format!("failed waiting for ffmpeg: {error}"),
        })?,
    };

    if !output_result.status.success() {
        let stderr = String::from_utf8_lossy(&output_result.stderr);
        return Err(MediaError::SidecarProcess {
            kind: SidecarKind::Ffmpeg,
            detail: stderr.trim().to_owned(),
        });
    }

    File::open(&temp_path)?.sync_all()?;
    temp_path.persist(output).map_err(|error| error.error)?;
    sync_directory(parent)?;
    Ok(())
}
