use std::{
    collections::{HashMap, HashSet},
    io,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use ruscello_core::{Download, Status};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::{
    archive::{
        ArchiveError, ExtractOptions, ExtractOutcome, extract_archive, parse_archive_volume,
    },
    automation::PostProcessingSettings,
    checksum_verify::{ChecksumVerifyError, SidecarKind, VerifiedFile, verify_for_completed},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostProcessStage {
    VerifyingChecksum,
    ExtractingArchive,
    RunningCommand,
}

#[derive(Debug, Clone)]
pub(crate) struct PostProcessJob {
    pub target_path: PathBuf,
    pub display_name: String,
    pub package: Option<String>,
    pub category: Option<String>,
    pub source_url: String,
    pub settings: PostProcessingSettings,
    pub should_extract: bool,
    pub keep_seeding: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PostProcessOutcome {
    pub verified: Vec<VerifiedFile>,
    pub extracted: Vec<ExtractOutcome>,
    pub hook_executed: bool,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PostProcessError {
    #[error("{0}")]
    Checksum(#[from] ChecksumVerifyError),
    #[error("{0}")]
    Archive(#[from] ArchiveError),
    #[error("post-download command syntax error: {0}")]
    InvalidCommand(String),
    #[error("post-download command failed with exit status {code:?}: {stderr}")]
    CommandFailed { code: Option<i32>, stderr: String },
    #[error("post-download command timed out after {secs}s")]
    CommandTimedOut { secs: u64 },
    #[error("post-download command failed to start: {0}")]
    CommandSpawn(io::Error),
    #[error("post-processing cancelled")]
    Cancelled,
}

pub(crate) async fn should_post_process(path: &Path, settings: &PostProcessingSettings) -> bool {
    if settings.post_download_command.is_some() {
        return true;
    }
    if settings.auto_verify_checksums && crate::checksum_verify::is_checksum_file(path) {
        return true;
    }
    if settings.auto_extract && parse_archive_volume(path).is_some() {
        return true;
    }
    if !settings.auto_verify_checksums && !settings.auto_extract {
        return false;
    }

    let path = path.to_path_buf();
    let auto_verify = settings.auto_verify_checksums;
    let auto_extract = settings.auto_extract;
    tokio::task::spawn_blocking(move || {
        has_actionable_post_process_files(&path, auto_verify, auto_extract)
    })
    .await
    .unwrap_or(false)
}

fn has_actionable_post_process_files(path: &Path, auto_verify: bool, auto_extract: bool) -> bool {
    if !path.is_dir() {
        return auto_verify && has_checksum_sidecar_in_parent(path);
    }
    if auto_verify && has_checksum_sidecar_in_parent(path) {
        return true;
    }

    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            let entry_path = entry.path();
            if ft.is_dir() {
                stack.push(entry_path);
            } else if ft.is_file()
                && ((auto_verify && crate::checksum_verify::is_checksum_file(&entry_path))
                    || (auto_extract && parse_archive_volume(&entry_path).is_some()))
            {
                return true;
            }
        }
    }
    false
}

fn has_checksum_sidecar_in_parent(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .file_type()
            .is_ok_and(|ft| ft.is_file() && crate::checksum_verify::is_checksum_file(&entry.path()))
    })
}

/// Checks whether a finished download belonging to an archive volume group is ready to
/// trigger extraction for the entire group (all package/directory siblings finished and the
/// group has not already been triggered in this session).
pub(crate) fn ready_for_archive_extraction(
    download: &Download,
    target_path: &Path,
    all_downloads: &[&Download],
    triggered_groups: &mut HashSet<String>,
) -> Option<bool> {
    let info = parse_archive_volume(target_path)?;
    if !info.is_multi_part {
        return Some(true);
    }
    if triggered_groups.contains(&info.group_key) {
        return Some(false);
    }

    let mut siblings = Vec::new();
    for &other in all_downloads {
        if other.id == download.id {
            continue;
        }
        let same_package = match (&download.package, &other.package) {
            (Some(a), Some(b)) => a == b,
            _ => other.destination == download.destination,
        };
        if !same_package {
            continue;
        }
        let Some(other_path) = predicted_download_path(other) else {
            continue;
        };
        if let Some(other_info) = parse_archive_volume(&other_path)
            && other_info.group_key == info.group_key
        {
            siblings.push(other);
        }
    }

    let all_other_finished = siblings.iter().all(|d| {
        matches!(
            d.status(),
            Status::Completed | Status::Seeding | Status::PostProcessing
        )
    });
    if !all_other_finished {
        return Some(false);
    }

    triggered_groups.insert(info.group_key);
    Some(true)
}

pub(crate) fn download_target_path(download: &Download) -> PathBuf {
    if let Some(completed) = &download.completed_path {
        completed.clone()
    } else if let Some(filename) = &download.filename {
        download.destination.join(filename)
    } else {
        download.destination.clone()
    }
}

fn predicted_download_path(download: &Download) -> Option<PathBuf> {
    if let Some(completed) = &download.completed_path {
        return Some(completed.clone());
    }
    if let Some(filename) = &download.filename {
        return Some(download.destination.join(filename));
    }
    let raw_url = match &download.source {
        ruscello_core::Source::Http { url }
        | ruscello_core::Source::Ftp { url }
        | ruscello_core::Source::Hoster { url }
        | ruscello_core::Source::Media { url } => url.as_str(),
        ruscello_core::Source::Torrent(_) => return None,
    };
    let parsed = ruscello_http::Url::parse(raw_url).ok()?;
    let segment = parsed
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|segment| !segment.is_empty())?;
    let decoded = percent_encoding::percent_decode_str(segment)
        .decode_utf8_lossy()
        .into_owned();
    (!decoded.is_empty()).then(|| download.destination.join(decoded))
}

pub(crate) async fn run_post_processing(
    job: PostProcessJob,
    cancellation: &CancellationToken,
    mut on_stage: impl FnMut(PostProcessStage) + Send,
) -> Result<PostProcessOutcome, PostProcessError> {
    if cancellation.is_cancelled() {
        return Err(PostProcessError::Cancelled);
    }

    let mut outcome = PostProcessOutcome::default();

    // 1. Automatic checksum verification (.sfv, .md5, .sha256)
    if job.settings.auto_verify_checksums {
        on_stage(PostProcessStage::VerifyingChecksum);
        outcome.verified = verify_target_path(&job.target_path, cancellation).await?;
    }

    // 2. Automatic archive extraction (.zip, .7z, .tar.*, optional external .rar)
    if job.settings.auto_extract && job.should_extract {
        let candidates = discover_archives_for_target(&job.target_path).await?;
        if !candidates.is_empty() {
            on_stage(PostProcessStage::ExtractingArchive);
            let options = ExtractOptions {
                extract_into_subfolder: job.settings.extract_into_subfolder,
                delete_archives_after_extraction: job.settings.delete_archives_after_extraction
                    && !job.keep_seeding,
                passwords: job.settings.archive_passwords.clone(),
                external_extractor_path: job.settings.external_extractor_path.clone(),
            };
            for archive_path in candidates {
                let extracted =
                    extract_archive(&archive_path, options.clone(), cancellation).await?;
                outcome.extracted.push(extracted);
            }
        }
    }

    // 3. Optional user command hook
    if let Some(template) = &job.settings.post_download_command {
        on_stage(PostProcessStage::RunningCommand);
        run_command_hook(
            template,
            &job,
            outcome
                .extracted
                .first()
                .map(|item| item.destination.as_path()),
            cancellation,
        )
        .await?;
        outcome.hook_executed = true;
    }

    Ok(outcome)
}

async fn verify_target_path(
    target_path: &Path,
    cancellation: &CancellationToken,
) -> Result<Vec<VerifiedFile>, ChecksumVerifyError> {
    if !target_path.is_dir() {
        return verify_for_completed(target_path, cancellation).await;
    }

    let root = target_path.to_path_buf();
    let manifests = tokio::task::spawn_blocking(move || collect_manifests_recursive(&root))
        .await
        .map_err(|err| ChecksumVerifyError::Io(io::Error::other(err)))?;
    let mut verified = Vec::new();
    for manifest in manifests {
        let mut partial = verify_for_completed(&manifest, cancellation).await?;
        verified.append(&mut partial);
    }
    Ok(verified)
}

fn collect_manifests_recursive(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() && SidecarKind::from_path(&path).is_some() {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

async fn discover_archives_for_target(
    target_path: &Path,
) -> Result<Vec<PathBuf>, PostProcessError> {
    if target_path.is_file() {
        if let Some(info) = parse_archive_volume(target_path) {
            if info.is_multi_part && !info.is_first {
                // Find the first volume path in the same directory when triggered by a late volume.
                let (_, volumes) = crate::archive::collect_disk_volumes(target_path)?;
                return Ok(volumes.into_iter().take(1).collect());
            }
            return Ok(vec![target_path.to_path_buf()]);
        }
        return Ok(Vec::new());
    }

    if !target_path.is_dir() {
        return Ok(Vec::new());
    }

    let dir = target_path.to_path_buf();
    let candidates = tokio::task::spawn_blocking(move || {
        let mut by_group: HashMap<String, (u32, PathBuf)> = HashMap::new();
        let mut stack = vec![dir];
        while let Some(current) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&current) else {
                continue;
            };
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.is_file()
                    && let Some(info) = parse_archive_volume(&path)
                {
                    let replace = by_group
                        .get(&info.group_key)
                        .is_none_or(|(idx, _)| info.part_index < *idx);
                    if replace {
                        by_group.insert(info.group_key, (info.part_index, path));
                    }
                }
            }
        }
        let mut out: Vec<PathBuf> = by_group.into_values().map(|(_, p)| p).collect();
        out.sort();
        out
    })
    .await
    .map_err(|err| ArchiveError::Io(io::Error::other(err)))?;

    Ok(candidates)
}

async fn run_command_hook(
    template: &str,
    job: &PostProcessJob,
    extracted_dir: Option<&Path>,
    cancellation: &CancellationToken,
) -> Result<(), PostProcessError> {
    let tokens = split_command_template(template)?;
    let Some(program_tpl) = tokens.first() else {
        return Err(PostProcessError::InvalidCommand(
            "command template is empty".to_owned(),
        ));
    };

    let parent_dir = if job.target_path.is_dir() {
        job.target_path.as_path()
    } else {
        job.target_path.parent().unwrap_or_else(|| Path::new("."))
    };
    let extracted_str = extracted_dir
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| parent_dir.to_string_lossy().into_owned());

    let replace = |raw: &str| -> String {
        raw.replace("{file}", &job.target_path.to_string_lossy())
            .replace("{dir}", &parent_dir.to_string_lossy())
            .replace("{name}", &job.display_name)
            .replace("{package}", job.package.as_deref().unwrap_or_default())
            .replace("{category}", job.category.as_deref().unwrap_or_default())
            .replace("{url}", &job.source_url)
            .replace("{extracted_dir}", &extracted_str)
    };

    let program = replace(program_tpl);
    let args: Vec<String> = tokens[1..].iter().map(|t| replace(t)).collect();

    let mut cmd = Command::new(&program);
    cmd.args(&args)
        .current_dir(parent_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("RUSCELLO_FILE", &job.target_path)
        .env("RUSCELLO_DIR", parent_dir)
        .env("RUSCELLO_NAME", &job.display_name)
        .env(
            "RUSCELLO_PACKAGE",
            job.package.as_deref().unwrap_or_default(),
        )
        .env(
            "RUSCELLO_CATEGORY",
            job.category.as_deref().unwrap_or_default(),
        )
        .env("RUSCELLO_URL", &job.source_url)
        .env("RUSCELLO_EXTRACTED_DIR", &extracted_str);

    let mut child = cmd.spawn().map_err(PostProcessError::CommandSpawn)?;
    let _stdout = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stderr_task = tokio::spawn(async move {
        use tokio::io::AsyncReadExt as _;
        let mut buf = Vec::new();
        if let Some(mut reader) = stderr_pipe {
            let _ = reader.read_to_end(&mut buf).await;
        }
        buf
    });
    let timeout_secs = job.settings.post_download_timeout_secs.max(1);
    let deadline = tokio::time::sleep(Duration::from_secs(timeout_secs));
    tokio::pin!(deadline);

    let status = tokio::select! {
        _ = cancellation.cancelled() => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(PostProcessError::Cancelled);
        }
        () = &mut deadline => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(PostProcessError::CommandTimedOut { secs: timeout_secs });
        }
        res = child.wait() => {
            res.map_err(PostProcessError::CommandSpawn)?
        }
    };

    if status.success() {
        Ok(())
    } else {
        let stderr_bytes = stderr_task.await.unwrap_or_default();
        let stderr = String::from_utf8_lossy(&stderr_bytes).trim().to_owned();
        Err(PostProcessError::CommandFailed {
            code: status.code(),
            stderr,
        })
    }
}

/// Splits a command line into arguments supporting single/double quotes and backslashes,
/// avoiding invocation of a system shell.
pub(crate) fn split_command_template(input: &str) -> Result<Vec<String>, PostProcessError> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut quote: Option<char> = None;
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            Some('"') => {
                if ch == '"' {
                    quote = None;
                } else if ch == '\\' {
                    match chars.peek().copied() {
                        Some('"' | '\\') => {
                            if let Some(escaped) = chars.next() {
                                current.push(escaped);
                            }
                        }
                        _ => current.push('\\'),
                    }
                } else {
                    current.push(ch);
                }
            }
            Some(_) => unreachable!(),
            None => {
                if ch.is_whitespace() {
                    if in_token {
                        args.push(std::mem::take(&mut current));
                        in_token = false;
                    }
                } else if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                    in_token = true;
                } else if ch == '\\' {
                    in_token = true;
                    if let Some(next) = chars.next() {
                        current.push(next);
                    } else {
                        return Err(PostProcessError::InvalidCommand(
                            "trailing backslash in command".to_owned(),
                        ));
                    }
                } else {
                    in_token = true;
                    current.push(ch);
                }
            }
        }
    }

    if quote.is_some() {
        return Err(PostProcessError::InvalidCommand(
            "unterminated quote in command".to_owned(),
        ));
    }
    if in_token {
        args.push(current);
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruscello_core::{DownloadId, PackageId, Source};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn dummy_download(id: u64, dir: &Path, name: &str, status: Status) -> Download {
        let mut d = Download::new(
            DownloadId(id),
            Source::Http {
                url: format!("https://example.test/{name}"),
            },
            dir.to_path_buf(),
        );
        d.filename = Some(name.to_owned());
        d.package = Some(PackageId(10));
        if status == Status::Completed {
            d.completed_path = Some(dir.join(name));
        }
        let _ = match status {
            Status::PostProcessing => d
                .transition(Status::Downloading)
                .and_then(|()| d.transition(Status::PostProcessing)),
            Status::Completed => d
                .transition(Status::Downloading)
                .and_then(|()| d.transition(Status::Completed)),
            Status::Downloading => d.transition(Status::Downloading),
            _ => Ok(()),
        };
        d
    }

    #[test]
    fn multipart_barrier_waits_until_all_package_parts_finish() {
        let dir = Path::new("/tmp/dl");
        let p1 = dummy_download(1, dir, "set.7z.001", Status::PostProcessing);
        let p2 = dummy_download(2, dir, "set.7z.002", Status::Downloading);
        let mut triggered = HashSet::new();
        let all = [&p1, &p2];
        assert_eq!(
            ready_for_archive_extraction(&p1, &dir.join("set.7z.001"), &all, &mut triggered),
            Some(false)
        );
        assert!(triggered.is_empty());

        // Now p1 is already Completed, and p2 finishes last -> p2 triggers group extraction once.
        let p1_done = dummy_download(1, dir, "set.7z.001", Status::Completed);
        let p2_done = dummy_download(2, dir, "set.7z.002", Status::PostProcessing);
        let all_done = [&p1_done, &p2_done];
        assert_eq!(
            ready_for_archive_extraction(
                &p2_done,
                &dir.join("set.7z.002"),
                &all_done,
                &mut triggered
            ),
            Some(true)
        );
        // Re-checking on another sibling returns false (already triggered).
        assert_eq!(
            ready_for_archive_extraction(
                &p1_done,
                &dir.join("set.7z.001"),
                &all_done,
                &mut triggered
            ),
            Some(false)
        );
    }

    #[test]
    fn command_splitter_preserves_quotes_and_rejects_unclosed() -> TestResult {
        let parsed = split_command_template(r#""/my dir/hook.sh" --name '{name}' {file}"#)?;
        assert_eq!(
            parsed,
            vec!["/my dir/hook.sh", "--name", "{name}", "{file}"]
        );
        assert!(split_command_template(r#"echo "unclosed"#).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn full_pipeline_verifies_checksum_extracts_and_runs_hook() -> TestResult {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        use zip::write::SimpleFileOptions;

        let dir = tempfile::tempdir()?;
        let zip_path = dir.path().join("bundle.zip");
        {
            let f = std::fs::File::create(&zip_path)?;
            let mut zw = zip::ZipWriter::new(f);
            zw.start_file("readme.md", SimpleFileOptions::default())?;
            zw.write_all(b"pipeline-ok")?;
            zw.finish()?;
        }

        let zip_bytes = std::fs::read(&zip_path)?;
        let sha_hex = {
            use sha2::Digest as _;
            hex::encode(sha2::Sha256::digest(&zip_bytes))
        };
        std::fs::write(
            dir.path().join("bundle.zip.sha256"),
            format!("{sha_hex}  bundle.zip\n"),
        )?;

        let marker = dir.path().join("hook-ran.txt");
        let script = dir.path().join("hook.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s|%s|%s' \"$1\" \"$RUSCELLO_NAME\" \"$RUSCELLO_PACKAGE\" > \"{}\"\n",
                marker.display()
            ),
        )?;
        let mut perms = std::fs::metadata(&script)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms)?;

        let job = PostProcessJob {
            target_path: zip_path.clone(),
            display_name: "bundle.zip".to_owned(),
            package: Some("Pkg".to_owned()),
            category: Some("Docs".to_owned()),
            source_url: "https://example.test/bundle.zip".to_owned(),
            settings: PostProcessingSettings {
                auto_verify_checksums: true,
                auto_extract: true,
                extract_into_subfolder: true,
                delete_archives_after_extraction: false,
                archive_passwords: Vec::new(),
                external_extractor_path: None,
                post_download_command: Some(format!("\"{}\" \"{{file}}\"", script.display())),
                post_download_timeout_secs: 15,
            },
            should_extract: true,
            keep_seeding: false,
        };

        let mut stages = Vec::new();
        let outcome = run_post_processing(job, &CancellationToken::new(), |stage| {
            stages.push(stage);
        })
        .await?;

        assert_eq!(
            stages,
            vec![
                PostProcessStage::VerifyingChecksum,
                PostProcessStage::ExtractingArchive,
                PostProcessStage::RunningCommand,
            ]
        );
        assert_eq!(outcome.verified.len(), 1);
        assert_eq!(outcome.extracted.len(), 1);
        assert!(outcome.hook_executed);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("bundle/readme.md"))?,
            "pipeline-ok"
        );
        assert_eq!(
            std::fs::read_to_string(&marker)?,
            format!("{}|bundle.zip|Pkg", zip_path.display())
        );
        Ok(())
    }
}
