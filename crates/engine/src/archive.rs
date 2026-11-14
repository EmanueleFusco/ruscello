use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Stdio,
    time::{SystemTime, UNIX_EPOCH},
};

use bzip2_rs::DecoderReader as Bz2Decoder;
use flate2::read::GzDecoder;
use sevenz_rust2::{ArchiveEntry, Password, decompress_with_extract_fn_and_password};
use tar::{Archive as TarArchive, EntryType};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use zip::{ZipArchive, result::ZipError};

use crate::checksum_verify::safe_relative_path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArchiveFormat {
    Zip,
    SevenZ,
    Tar,
    TarGz,
    TarXz,
    TarLzma,
    TarBz2,
    Rar,
}

impl ArchiveFormat {
    pub(crate) const fn is_rar(self) -> bool {
        matches!(self, Self::Rar)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArchiveVolumeInfo {
    pub format: ArchiveFormat,
    pub group_key: String,
    pub archive_stem: String,
    pub part_index: u32,
    pub is_multi_part: bool,
    pub is_first: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtractOutcome {
    pub destination: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ArchiveError {
    #[error("path is not a supported archive: {0}")]
    UnsupportedFormat(String),
    #[error("multi-part archive is missing one or more volumes ({0})")]
    IncompleteVolumes(String),
    #[error("archive requires a password or none of the configured passwords matched")]
    PasswordRequiredOrWrong,
    #[error("archive contains unsafe entry path or link: {0}")]
    UnsafeEntry(String),
    #[error("RAR extraction requires an external `7z`, `7zz`, or `unrar` binary (see ADR-0010)")]
    ExternalExtractorMissing,
    #[error("external extractor `{tool}` failed: {message}")]
    ExternalExtractorFailed { tool: String, message: String },
    #[error("archive extraction failed: {0}")]
    CorruptOrUnsupported(String),
    #[error("archive extraction cancelled")]
    Cancelled,
    #[error("archive I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Parses an archive path to identify its format and optional multi-part group metadata.
pub(crate) fn parse_archive_volume(path: &Path) -> Option<ArchiveVolumeInfo> {
    let file_name = path.file_name()?.to_str()?;
    let lower = file_name.to_ascii_lowercase();
    let dir_prefix = path
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Ignore partial Ruscello temporary files.
    if lower.ends_with(".rscpart") || lower.ends_with(".rscpart.ctl") {
        return None;
    }

    // 1. Generic raw-concat split numeric volumes: `foo.7z.001`, `foo.zip.001`, `foo.tar.xz.001`, ...
    if let Some((base_name, digits_str)) = file_name.rsplit_once('.')
        && digits_str.len() >= 3
        && digits_str.bytes().all(|b| b.is_ascii_digit())
        && let Ok(part_index) = digits_str.parse::<u32>()
        && let Some((format, stem)) = match_single_extension(base_name)
    {
        let group_key = format!("{dir_prefix}::{}::split", base_name.to_ascii_lowercase());
        return Some(ArchiveVolumeInfo {
            format,
            group_key,
            archive_stem: stem.to_owned(),
            part_index,
            is_multi_part: true,
            is_first: part_index == 1,
        });
    }

    // 2. Modern RAR multi-part: `name.part1.rar`, `name.part01.rar`, `name.part001.rar`
    if let Some(without_rar) = strip_suffix_ci(file_name, ".rar")
        && let Some((base_stem, part_tag)) = without_rar.rsplit_once('.')
        && let Some(digits) = strip_prefix_ci(part_tag, "part")
        && !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && let Ok(part_index) = digits.parse::<u32>()
        && !base_stem.is_empty()
    {
        let group_key = format!("{dir_prefix}::{}::rar-part", base_stem.to_ascii_lowercase());
        return Some(ArchiveVolumeInfo {
            format: ArchiveFormat::Rar,
            group_key,
            archive_stem: base_stem.to_owned(),
            part_index,
            is_multi_part: true,
            is_first: part_index == 1,
        });
    }

    // 3. Classic RAR split secondary volumes: `name.r00`, `name.r01`, ...
    if let Some((base_stem, ext)) = file_name.rsplit_once('.')
        && ext.len() == 3
        && (ext.starts_with('r') || ext.starts_with('R'))
        && ext.as_bytes()[1..].iter().all(u8::is_ascii_digit)
        && let Ok(vol_num) = ext[1..].parse::<u32>()
        && !base_stem.is_empty()
    {
        let group_key = format!("{dir_prefix}::{}::rar-old", base_stem.to_ascii_lowercase());
        return Some(ArchiveVolumeInfo {
            format: ArchiveFormat::Rar,
            group_key,
            archive_stem: base_stem.to_owned(),
            part_index: vol_num + 1,
            is_multi_part: true,
            is_first: false,
        });
    }

    // 4. Single archive extensions (including `.rar`, which may have `.r00` siblings).
    let (format, stem) = match_single_extension(file_name)?;
    if format == ArchiveFormat::Rar {
        let group_key = if has_classic_rar_parts(path, stem) {
            format!("{dir_prefix}::{}::rar-old", stem.to_ascii_lowercase())
        } else {
            format!("{dir_prefix}::{}::single-rar", lower)
        };
        let is_multi_part = group_key.ends_with("::rar-old");
        return Some(ArchiveVolumeInfo {
            format,
            group_key,
            archive_stem: stem.to_owned(),
            part_index: 0,
            is_multi_part,
            is_first: true,
        });
    }

    Some(ArchiveVolumeInfo {
        format,
        group_key: format!("{dir_prefix}::{lower}::single"),
        archive_stem: stem.to_owned(),
        part_index: 1,
        is_multi_part: false,
        is_first: true,
    })
}

fn has_classic_rar_parts(rar_path: &Path, stem: &str) -> bool {
    let Some(parent) = rar_path.parent() else {
        return false;
    };
    let expected_r00 = format!("{}.r00", stem.to_ascii_lowercase());
    let Ok(entries) = fs::read_dir(parent) else {
        return false;
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .any(|name| name.to_ascii_lowercase() == expected_r00)
}

fn match_single_extension(file_name: &str) -> Option<(ArchiveFormat, &str)> {
    const TABLE: &[(&str, ArchiveFormat)] = &[
        (".tar.gz", ArchiveFormat::TarGz),
        (".tar.xz", ArchiveFormat::TarXz),
        (".tar.lzma", ArchiveFormat::TarLzma),
        (".tar.bz2", ArchiveFormat::TarBz2),
        (".tgz", ArchiveFormat::TarGz),
        (".txz", ArchiveFormat::TarXz),
        (".tbz2", ArchiveFormat::TarBz2),
        (".tb2", ArchiveFormat::TarBz2),
        (".zip", ArchiveFormat::Zip),
        (".7z", ArchiveFormat::SevenZ),
        (".tar", ArchiveFormat::Tar),
        (".rar", ArchiveFormat::Rar),
    ];

    for &(suffix, format) in TABLE {
        if let Some(stem) = strip_suffix_ci(file_name, suffix)
            && !stem.is_empty()
        {
            return Some((format, stem));
        }
    }
    None
}

fn strip_suffix_ci<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    if text.len() < suffix.len() {
        return None;
    }
    let split = text.len() - suffix.len();
    text.is_char_boundary(split)
        .then(|| {
            text[split..]
                .eq_ignore_ascii_case(suffix)
                .then_some(&text[..split])
        })
        .flatten()
}

fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text.len() < prefix.len() {
        return None;
    }
    text.is_char_boundary(prefix.len())
        .then(|| {
            text[..prefix.len()]
                .eq_ignore_ascii_case(prefix)
                .then_some(&text[prefix.len()..])
        })
        .flatten()
}

pub(crate) fn collect_disk_volumes(
    archive_path: &Path,
) -> Result<(ArchiveVolumeInfo, Vec<PathBuf>), ArchiveError> {
    let info = parse_archive_volume(archive_path)
        .ok_or_else(|| ArchiveError::UnsupportedFormat(archive_path.display().to_string()))?;

    if !info.is_multi_part {
        if !archive_path.is_file() {
            return Err(ArchiveError::IncompleteVolumes(
                archive_path.display().to_string(),
            ));
        }
        return Ok((info, vec![archive_path.to_path_buf()]));
    }

    let parent = archive_path
        .parent()
        .ok_or_else(|| ArchiveError::IncompleteVolumes(archive_path.display().to_string()))?;

    let mut matched: Vec<(u32, PathBuf)> = fs::read_dir(parent)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|candidate| candidate.is_file())
        .filter_map(|candidate| {
            let parsed = parse_archive_volume(&candidate)?;
            (parsed.group_key == info.group_key).then_some((parsed.part_index, candidate))
        })
        .collect();

    matched.sort_by_key(|(idx, _)| *idx);
    if matched.is_empty() {
        return Err(ArchiveError::IncompleteVolumes(
            archive_path.display().to_string(),
        ));
    }

    // Ensure contiguous numbering without gaps starting from the expected initial volume index.
    let expected_start = if info.group_key.ends_with("::rar-old") {
        0
    } else {
        1
    };
    for (pos, (part_index, _)) in matched.iter().enumerate() {
        if *part_index != expected_start + pos as u32 {
            return Err(ArchiveError::IncompleteVolumes(format!(
                "missing volume {} for {}",
                expected_start + pos as u32,
                info.archive_stem
            )));
        }
    }

    let first_path = matched[0].1.clone();
    let first_info = parse_archive_volume(&first_path)
        .ok_or_else(|| ArchiveError::UnsupportedFormat(first_path.display().to_string()))?;
    let paths = matched.into_iter().map(|(_, path)| path).collect();
    Ok((first_info, paths))
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ExtractOptions {
    pub extract_into_subfolder: bool,
    pub delete_archives_after_extraction: bool,
    pub passwords: Vec<String>,
    pub external_extractor_path: Option<PathBuf>,
}

pub(crate) async fn extract_archive(
    volume_path: &Path,
    options: ExtractOptions,
    cancellation: &CancellationToken,
) -> Result<ExtractOutcome, ArchiveError> {
    if cancellation.is_cancelled() {
        return Err(ArchiveError::Cancelled);
    }

    let path_owned = volume_path.to_path_buf();
    let (info, volumes) = tokio::task::spawn_blocking(move || collect_disk_volumes(&path_owned))
        .await
        .map_err(|err| ArchiveError::Io(io::Error::other(err)))??;

    let first_volume = volumes
        .first()
        .cloned()
        .ok_or_else(|| ArchiveError::IncompleteVolumes(volume_path.display().to_string()))?;
    let parent_dir = first_volume
        .parent()
        .ok_or_else(|| ArchiveError::Io(io::Error::other("archive path has no parent directory")))?
        .to_path_buf();
    let final_dest = if options.extract_into_subfolder {
        parent_dir.join(&info.archive_stem)
    } else {
        parent_dir.clone()
    };

    let staging_dir = make_staging_dir(&parent_dir)?;
    let _cleanup_guard = StagingGuard(Some(staging_dir.clone()));

    let _extracted_files = if info.format.is_rar() {
        extract_rar_with_sidecar(
            &first_volume,
            &staging_dir,
            &options.passwords,
            options.external_extractor_path.as_deref(),
            cancellation,
        )
        .await?
    } else {
        let volumes_clone = volumes.clone();
        let passwords_clone = options.passwords.clone();
        let staging_clone = staging_dir.clone();
        let cancel_clone = cancellation.clone();
        let format = info.format;
        tokio::task::spawn_blocking(move || {
            extract_pure_rust(
                format,
                &volumes_clone,
                &staging_clone,
                &passwords_clone,
                &cancel_clone,
            )
        })
        .await
        .map_err(|err| ArchiveError::Io(io::Error::other(err)))??
    };

    if cancellation.is_cancelled() {
        return Err(ArchiveError::Cancelled);
    }

    let staging_verify = staging_dir.clone();
    let dest_clone = final_dest.clone();
    let delete_volumes = if options.delete_archives_after_extraction {
        volumes.clone()
    } else {
        Vec::new()
    };
    let _deleted_volumes =
        tokio::task::spawn_blocking(move || -> Result<Vec<PathBuf>, ArchiveError> {
            ensure_no_symlinks(&staging_verify)?;
            fs::create_dir_all(&dest_clone)?;
            move_directory_contents(&staging_verify, &dest_clone)?;
            let mut deleted = Vec::new();
            for vol in delete_volumes {
                if vol.exists() {
                    fs::remove_file(&vol)?;
                    deleted.push(vol);
                }
            }
            Ok(deleted)
        })
        .await
        .map_err(|err| ArchiveError::Io(io::Error::other(err)))??;

    Ok(ExtractOutcome {
        destination: final_dest,
    })
}

struct StagingGuard(Option<PathBuf>);

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if let Some(dir) = self.0.take()
            && dir.exists()
        {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

fn make_staging_dir(parent_dir: &Path) -> Result<PathBuf, ArchiveError> {
    fs::create_dir_all(parent_dir)?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = parent_dir.join(format!(".ruscello-extract-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn extract_pure_rust(
    format: ArchiveFormat,
    volumes: &[PathBuf],
    staging_dir: &Path,
    passwords: &[String],
    cancellation: &CancellationToken,
) -> Result<usize, ArchiveError> {
    match format {
        ArchiveFormat::Zip => extract_zip(volumes, staging_dir, passwords, cancellation),
        ArchiveFormat::SevenZ => extract_7z(volumes, staging_dir, passwords, cancellation),
        ArchiveFormat::Tar => {
            let reader = ConcatReader::open(volumes)?;
            extract_tar_stream(reader, staging_dir, cancellation)
        }
        ArchiveFormat::TarGz => {
            let reader = ConcatReader::open(volumes)?;
            extract_tar_stream(GzDecoder::new(reader), staging_dir, cancellation)
        }
        ArchiveFormat::TarBz2 => {
            let reader = ConcatReader::open(volumes)?;
            extract_tar_stream(Bz2Decoder::new(reader), staging_dir, cancellation)
        }
        ArchiveFormat::TarXz | ArchiveFormat::TarLzma => {
            let mut reader = BufReader::new(ConcatReader::open(volumes)?);
            let mut decoded_tar = tempfile::tempfile()?;
            if format == ArchiveFormat::TarXz {
                lzma_rs::xz_decompress(&mut reader, &mut decoded_tar)
                    .map_err(|err| ArchiveError::CorruptOrUnsupported(err.to_string()))?;
            } else {
                lzma_rs::lzma_decompress(&mut reader, &mut decoded_tar)
                    .map_err(|err| ArchiveError::CorruptOrUnsupported(err.to_string()))?;
            }
            decoded_tar.seek(SeekFrom::Start(0))?;
            extract_tar_stream(decoded_tar, staging_dir, cancellation)
        }
        ArchiveFormat::Rar => Err(ArchiveError::UnsupportedFormat("rar".to_owned())),
    }
}

fn extract_zip(
    volumes: &[PathBuf],
    staging_dir: &Path,
    passwords: &[String],
    cancellation: &CancellationToken,
) -> Result<usize, ArchiveError> {
    let reader = ConcatReader::open(volumes)?;
    let mut archive = ZipArchive::new(reader)
        .map_err(|err| ArchiveError::CorruptOrUnsupported(err.to_string()))?;
    let mut extracted_files = 0_usize;

    for index in 0..archive.len() {
        if cancellation.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let raw_name = {
            let entry = archive
                .by_index_decrypt(index, &[])
                .map_err(|err| ArchiveError::CorruptOrUnsupported(err.to_string()))?;
            if is_zip_symlink(entry.unix_mode()) {
                let err_name = entry
                    .name()
                    .map(|s| s.into_owned())
                    .unwrap_or_else(|_| format!("entry-{index}"));
                return Err(ArchiveError::UnsafeEntry(err_name));
            }
            let name = entry
                .name()
                .map(|s| s.into_owned())
                .map_err(|err| ArchiveError::UnsafeEntry(err.to_string()))?;
            let is_dir = entry.is_dir();
            drop(entry);
            if is_dir {
                let Some(rel) = safe_relative_path(&name) else {
                    return Err(ArchiveError::UnsafeEntry(name));
                };
                fs::create_dir_all(staging_dir.join(rel))?;
                continue;
            }
            name
        };

        let Some(rel) = safe_relative_path(&raw_name) else {
            return Err(ArchiveError::UnsafeEntry(raw_name));
        };
        let target_path = staging_dir.join(rel);
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // First try without password, then iterate through configured passwords if encrypted.
        let mut success = false;
        let mut saw_encryption = false;
        let candidates: Vec<&[u8]> = std::iter::once(&[][..])
            .chain(passwords.iter().map(String::as_bytes))
            .collect();

        for (attempt, secret) in candidates.iter().enumerate() {
            match archive.by_index_decrypt(index, secret) {
                Ok(mut entry) => {
                    let mut out = File::create(&target_path)?;
                    match copy_cancellable(&mut entry, &mut out, cancellation) {
                        Ok(()) => {
                            success = true;
                            break;
                        }
                        Err(ArchiveError::Io(err))
                            if attempt == 0 && looks_like_bad_zip_pw(&err) =>
                        {
                            saw_encryption = true;
                            let _ = fs::remove_file(&target_path);
                        }
                        Err(ArchiveError::Io(err))
                            if attempt > 0 && looks_like_bad_zip_pw(&err) =>
                        {
                            saw_encryption = true;
                            let _ = fs::remove_file(&target_path);
                        }
                        Err(other) => return Err(other),
                    }
                }
                Err(ZipError::UnsupportedArchive(ZipError::PASSWORD_REQUIRED))
                | Err(ZipError::InvalidPassword) => {
                    saw_encryption = true;
                }
                Err(err) => {
                    return Err(ArchiveError::CorruptOrUnsupported(err.to_string()));
                }
            }
        }

        if !success {
            return Err(if saw_encryption {
                ArchiveError::PasswordRequiredOrWrong
            } else {
                ArchiveError::CorruptOrUnsupported(format!("failed to decrypt entry {raw_name}"))
            });
        }
        extracted_files += 1;
    }

    Ok(extracted_files)
}

fn is_zip_symlink(unix_mode: Option<u32>) -> bool {
    unix_mode.is_some_and(|mode| (mode & 0o17_0000) == 0o12_0000)
}

fn looks_like_bad_zip_pw(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::InvalidData
        || err.to_string().to_ascii_lowercase().contains("password")
}

fn extract_7z(
    volumes: &[PathBuf],
    staging_dir: &Path,
    passwords: &[String],
    cancellation: &CancellationToken,
) -> Result<usize, ArchiveError> {
    let mut attempts: Vec<Password> = std::iter::once(Password::empty())
        .chain(passwords.iter().map(|pw| Password::from(pw.as_str())))
        .collect();
    // Deduplicate empty passwords cleanly if user included "".
    attempts.dedup_by(|a, b| a.as_slice() == b.as_slice());

    let mut last_err: Option<ArchiveError> = None;
    for (idx, password) in attempts.iter().enumerate() {
        if cancellation.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        clean_dir_contents(staging_dir)?;
        let reader = ConcatReader::open(volumes)?;
        let mut file_count = 0_usize;
        let cancel_inner = cancellation.clone();
        let staging_root = staging_dir.to_path_buf();

        let res = decompress_with_extract_fn_and_password(
            reader,
            &staging_root,
            password.clone(),
            |entry: &ArchiveEntry, entry_reader: &mut dyn Read, _dest: &PathBuf| {
                if cancel_inner.is_cancelled() {
                    return Err(sevenz_rust2::Error::Other("cancelled".into()));
                }
                if is_7z_symlink(entry) {
                    return Err(sevenz_rust2::Error::Other(
                        format!("unsafe symlink entry: {}", entry.name()).into(),
                    ));
                }
                let Some(rel) = safe_relative_path(entry.name()) else {
                    return Err(sevenz_rust2::Error::Other(
                        format!("unsafe path entry: {}", entry.name()).into(),
                    ));
                };
                let target = staging_root.join(rel);
                if entry.is_directory() {
                    fs::create_dir_all(&target)?;
                    return Ok(true);
                }
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut out = File::create(&target)?;
                let mut buf = [0_u8; 32 * 1024];
                loop {
                    if cancel_inner.is_cancelled() {
                        return Err(sevenz_rust2::Error::Other("cancelled".into()));
                    }
                    let n = entry_reader.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    use std::io::Write as _;
                    out.write_all(&buf[..n])?;
                }
                file_count += 1;
                Ok(true)
            },
        );

        match res {
            Ok(()) => return Ok(file_count),
            Err(sevenz_rust2::Error::Other(msg)) if msg == "cancelled" => {
                return Err(ArchiveError::Cancelled);
            }
            Err(sevenz_rust2::Error::Other(msg)) if msg.starts_with("unsafe ") => {
                return Err(ArchiveError::UnsafeEntry(msg.into_owned()));
            }
            Err(
                sevenz_rust2::Error::PasswordRequired | sevenz_rust2::Error::MaybeBadPassword(_),
            ) => {
                last_err = Some(ArchiveError::PasswordRequiredOrWrong);
                continue;
            }
            Err(
                ref err @ (sevenz_rust2::Error::ChecksumVerificationFailed
                | sevenz_rust2::Error::NextHeaderCrcMismatch),
            ) => {
                last_err = Some(if !passwords.is_empty() || idx > 0 {
                    ArchiveError::PasswordRequiredOrWrong
                } else {
                    ArchiveError::CorruptOrUnsupported(err.to_string())
                });
                continue;
            }
            Err(err) => {
                last_err = Some(ArchiveError::CorruptOrUnsupported(err.to_string()));
                break;
            }
        }
    }

    Err(last_err.unwrap_or_else(|| ArchiveError::CorruptOrUnsupported("7z failed".to_owned())))
}

fn is_7z_symlink(entry: &ArchiveEntry) -> bool {
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    (entry.windows_attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0
}

fn extract_tar_stream<R: Read>(
    reader: R,
    staging_dir: &Path,
    cancellation: &CancellationToken,
) -> Result<usize, ArchiveError> {
    let mut archive = TarArchive::new(reader);
    let mut extracted_files = 0_usize;

    let entries = archive
        .entries()
        .map_err(|err| ArchiveError::CorruptOrUnsupported(err.to_string()))?;
    for entry_res in entries {
        if cancellation.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let mut entry =
            entry_res.map_err(|err| ArchiveError::CorruptOrUnsupported(err.to_string()))?;
        let entry_type = entry.header().entry_type();
        let raw_path = entry
            .path()
            .map_err(|err| ArchiveError::UnsafeEntry(err.to_string()))?
            .to_string_lossy()
            .into_owned();

        if matches!(entry_type, EntryType::Symlink | EntryType::Link) {
            return Err(ArchiveError::UnsafeEntry(raw_path));
        }

        let Some(rel) = safe_relative_path(&raw_path) else {
            return Err(ArchiveError::UnsafeEntry(raw_path));
        };
        let target = staging_dir.join(rel);

        if entry_type.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if !entry_type.is_file() {
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = File::create(&target)?;
        copy_cancellable(&mut entry, &mut out, cancellation)?;
        extracted_files += 1;
    }

    Ok(extracted_files)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalToolFlavor {
    SevenZip,
    Unrar,
}

#[derive(Debug, Clone)]
struct ExternalTool {
    path: PathBuf,
    flavor: ExternalToolFlavor,
}

fn resolve_external_rar_tool(configured: Option<&Path>) -> Option<ExternalTool> {
    if let Some(custom) = configured {
        let resolved = if custom.is_absolute() || custom.components().count() > 1 {
            custom.exists().then(|| custom.to_path_buf())
        } else {
            find_in_path(custom)
        }?;
        return Some(ExternalTool {
            flavor: detect_flavor(&resolved),
            path: resolved,
        });
    }

    for candidate in ["7zz", "7z", "unrar"] {
        if let Some(found) = find_in_path(Path::new(candidate)) {
            return Some(ExternalTool {
                flavor: detect_flavor(&found),
                path: found,
            });
        }
    }

    #[cfg(target_os = "windows")]
    {
        for known in [
            r"C:\Program Files\7-Zip\7z.exe",
            r"C:\Program Files (x86)\7-Zip\7z.exe",
            r"C:\Program Files\WinRAR\UnRAR.exe",
        ] {
            let candidate = Path::new(known);
            if candidate.is_file() {
                return Some(ExternalTool {
                    flavor: detect_flavor(candidate),
                    path: candidate.to_path_buf(),
                });
            }
        }
    }

    None
}

fn detect_flavor(path: &Path) -> ExternalToolFlavor {
    let stem = path
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if stem.contains("unrar") {
        ExternalToolFlavor::Unrar
    } else {
        ExternalToolFlavor::SevenZip
    }
}

fn find_in_path(binary: &Path) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
        #[cfg(target_os = "windows")]
        {
            if candidate.extension().is_none() {
                let with_exe = candidate.with_extension("exe");
                if is_executable_file(&with_exe) {
                    return Some(with_exe);
                }
            }
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    #[cfg(unix)]
    {
        if !path.is_file() {
            return false;
        }
        use std::os::unix::fs::PermissionsExt as _;
        fs::metadata(path)
            .map(|meta| (meta.permissions().mode() & 0o111) != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

async fn extract_rar_with_sidecar(
    first_volume: &Path,
    staging_dir: &Path,
    passwords: &[String],
    configured_tool: Option<&Path>,
    cancellation: &CancellationToken,
) -> Result<usize, ArchiveError> {
    let tool =
        resolve_external_rar_tool(configured_tool).ok_or(ArchiveError::ExternalExtractorMissing)?;

    let mut attempts: Vec<Option<&str>> = vec![None];
    for pw in passwords {
        attempts.push(Some(pw.as_str()));
    }

    let mut last_message = String::new();
    let mut password_failure = false;

    for (index, secret) in attempts.iter().enumerate() {
        if cancellation.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let staging_clean = staging_dir.to_path_buf();
        tokio::task::spawn_blocking(move || clean_dir_contents(&staging_clean))
            .await
            .map_err(|err| ArchiveError::Io(io::Error::other(err)))??;

        let mut cmd = Command::new(&tool.path);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        match tool.flavor {
            ExternalToolFlavor::SevenZip => {
                cmd.arg("x").arg("-y").arg("-bd");
                cmd.arg(format!("-p{}", secret.unwrap_or("")));
                cmd.arg(format!("-o{}", staging_dir.display()));
                cmd.arg("--");
                cmd.arg(first_volume);
            }
            ExternalToolFlavor::Unrar => {
                cmd.arg("x").arg("-y").arg("-o+");
                match secret {
                    Some(pw) => {
                        cmd.arg(format!("-p{pw}"));
                    }
                    None => {
                        cmd.arg("-p-");
                    }
                }
                cmd.arg("--");
                cmd.arg(first_volume);
                let mut out_arg = staging_dir.as_os_str().to_os_string();
                out_arg.push(std::path::MAIN_SEPARATOR_STR);
                cmd.arg(out_arg);
            }
        }

        let mut child = cmd
            .spawn()
            .map_err(|err| ArchiveError::ExternalExtractorFailed {
                tool: tool.path.display().to_string(),
                message: err.to_string(),
            })?;
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let stdout_task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt as _;
            let mut buf = Vec::new();
            if let Some(mut reader) = stdout_pipe {
                let _ = reader.read_to_end(&mut buf).await;
            }
            buf
        });
        let stderr_task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt as _;
            let mut buf = Vec::new();
            if let Some(mut reader) = stderr_pipe {
                let _ = reader.read_to_end(&mut buf).await;
            }
            buf
        });

        let status = tokio::select! {
            _ = cancellation.cancelled() => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(ArchiveError::Cancelled);
            }
            res = child.wait() => res?,
        };

        if status.success() {
            let count_dir = staging_dir.to_path_buf();
            return tokio::task::spawn_blocking(move || count_regular_files(&count_dir))
                .await
                .map_err(|err| ArchiveError::Io(io::Error::other(err)))?;
        }

        let stdout_bytes = stdout_task.await.unwrap_or_default();
        let stderr_bytes = stderr_task.await.unwrap_or_default();
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&stdout_bytes),
            String::from_utf8_lossy(&stderr_bytes)
        );
        let lower = combined.to_ascii_lowercase();
        let looks_like_pw = lower.contains("password") || lower.contains("wrong password");
        if looks_like_pw {
            password_failure = true;
            last_message = combined.trim().to_owned();
            if index + 1 < attempts.len() {
                continue;
            }
        } else {
            last_message = combined.trim().to_owned();
            if index == 0 && attempts.len() > 1 && lower.contains("encrypted") {
                password_failure = true;
                continue;
            }
            break;
        }
    }

    if password_failure {
        Err(ArchiveError::PasswordRequiredOrWrong)
    } else {
        Err(ArchiveError::ExternalExtractorFailed {
            tool: tool.path.display().to_string(),
            message: if last_message.is_empty() {
                "non-zero exit code".to_owned()
            } else {
                last_message
            },
        })
    }
}

fn copy_cancellable(
    src: &mut impl Read,
    dst: &mut impl io::Write,
    cancellation: &CancellationToken,
) -> Result<(), ArchiveError> {
    let mut buf = [0_u8; 32 * 1024];
    loop {
        if cancellation.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let read = src.read(&mut buf)?;
        if read == 0 {
            break;
        }
        dst.write_all(&buf[..read])?;
    }
    Ok(())
}

fn ensure_no_symlinks(dir: &Path) -> Result<(), ArchiveError> {
    for entry_res in fs::read_dir(dir)? {
        let entry = entry_res?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            return Err(ArchiveError::UnsafeEntry(path.display().to_string()));
        }
        if meta.is_dir() {
            ensure_no_symlinks(&path)?;
        }
    }
    Ok(())
}

fn count_regular_files(dir: &Path) -> Result<usize, ArchiveError> {
    let mut count = 0_usize;
    for entry_res in fs::read_dir(dir)? {
        let entry = entry_res?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            count += count_regular_files(&path)?;
        } else if meta.is_file() {
            count += 1;
        }
    }
    Ok(count)
}

fn clean_dir_contents(dir: &Path) -> io::Result<()> {
    for entry_res in fs::read_dir(dir)? {
        let entry = entry_res?;
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn move_directory_contents(src_dir: &Path, dst_dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dst_dir)?;
    for entry_res in fs::read_dir(src_dir)? {
        let entry = entry_res?;
        let src_path = entry.path();
        let dst_path = dst_dir.join(entry.file_name());
        let meta = fs::symlink_metadata(&src_path)?;
        if meta.is_dir() {
            if !dst_path.exists() && fs::rename(&src_path, &dst_path).is_ok() {
                continue;
            }
            move_directory_contents(&src_path, &dst_path)?;
            let _ = fs::remove_dir_all(&src_path);
        } else if meta.is_file() {
            if dst_path.is_file() {
                fs::remove_file(&dst_path)?;
            }
            if let Err(_err) = fs::rename(&src_path, &dst_path) {
                fs::copy(&src_path, &dst_path)?;
                fs::remove_file(&src_path)?;
            }
        }
    }
    Ok(())
}

/// Streams a sequence of raw split volume files (`.001`, `.002`, ...) as one contiguous `Read + Seek`.
pub(crate) struct ConcatReader {
    files: Vec<File>,
    offsets: Vec<u64>,
    total_len: u64,
    position: u64,
    active_index: usize,
}

impl ConcatReader {
    pub(crate) fn open(paths: &[PathBuf]) -> io::Result<Self> {
        if paths.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot open ConcatReader with empty volume list",
            ));
        }
        let mut files = Vec::with_capacity(paths.len());
        let mut offsets = Vec::with_capacity(paths.len());
        let mut running = 0_u64;
        for path in paths {
            let file = File::open(path)?;
            let len = file.metadata()?.len();
            offsets.push(running);
            running = running
                .checked_add(len)
                .ok_or_else(|| io::Error::other("combined archive length overflow"))?;
            files.push(file);
        }
        Ok(Self {
            files,
            offsets,
            total_len: running,
            position: 0,
            active_index: 0,
        })
    }

    fn locate_active(&mut self) -> io::Result<usize> {
        if self.position >= self.total_len {
            return Ok(self.files.len());
        }
        let idx = match self.offsets.binary_search(&self.position) {
            Ok(exact) => exact,
            Err(insert_at) => insert_at.saturating_sub(1),
        };
        // Skip zero-length volumes cleanly if any exist.
        let mut idx = idx;
        while idx + 1 < self.offsets.len() && self.offsets[idx + 1] <= self.position {
            idx += 1;
        }
        let local_offset = self.position - self.offsets[idx];
        self.files[idx].seek(SeekFrom::Start(local_offset))?;
        self.active_index = idx;
        Ok(idx)
    }
}

impl Read for ConcatReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.position >= self.total_len {
            return Ok(0);
        }
        let mut idx = self.active_index;
        let expected_end = if idx + 1 < self.offsets.len() {
            self.offsets[idx + 1]
        } else {
            self.total_len
        };
        if self.position < self.offsets[idx] || self.position >= expected_end {
            idx = self.locate_active()?;
        }
        while idx < self.files.len() {
            let n = self.files[idx].read(buf)?;
            if n > 0 {
                self.position += n as u64;
                return Ok(n);
            }
            idx += 1;
            if idx < self.files.len() {
                self.files[idx].seek(SeekFrom::Start(0))?;
                self.active_index = idx;
            }
        }
        Ok(0)
    }
}

impl Seek for ConcatReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let next: i128 = match pos {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::End(delta) => i128::from(self.total_len) + i128::from(delta),
            SeekFrom::Current(delta) => i128::from(self.position) + i128::from(delta),
        };
        if next < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot seek before start of concatenated stream",
            ));
        }
        self.position = u64::try_from(next)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "seek offset overflow"))?;
        self.locate_active()?;
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use tar::Builder as TarBuilder;
    use zip::write::SimpleFileOptions;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn concat_reader_reads_and_seeks_across_split_parts() -> TestResult {
        let dir = tempfile::tempdir()?;
        let p1 = dir.path().join("blob.001");
        let p2 = dir.path().join("blob.002");
        let p3 = dir.path().join("blob.003");
        fs::write(&p1, b"AAAA")?;
        fs::write(&p2, b"BBBBBB")?;
        fs::write(&p3, b"CC")?;

        let mut reader = ConcatReader::open(&[p1, p2, p3])?;
        let mut all = String::new();
        reader.read_to_string(&mut all)?;
        assert_eq!(all, "AAAABBBBBBCC");

        reader.seek(SeekFrom::Start(3))?;
        let mut mid = [0_u8; 4];
        reader.read_exact(&mut mid)?;
        assert_eq!(&mid, b"ABBB");

        reader.seek(SeekFrom::End(-3))?;
        let mut tail = String::new();
        reader.read_to_string(&mut tail)?;
        assert_eq!(tail, "BCC");
        Ok(())
    }

    #[test]
    fn parses_single_and_multipart_archive_names() -> TestResult {
        let tar_gz = parse_archive_volume(Path::new("/dl/dist.tar.gz")).ok_or("tar.gz")?;
        assert_eq!(tar_gz.format, ArchiveFormat::TarGz);
        assert_eq!(tar_gz.archive_stem, "dist");
        assert!(!tar_gz.is_multi_part);

        let split_7z = parse_archive_volume(Path::new("/dl/release.7z.002")).ok_or("7z.002")?;
        assert_eq!(split_7z.format, ArchiveFormat::SevenZ);
        assert_eq!(split_7z.archive_stem, "release");
        assert!(split_7z.is_multi_part);
        assert_eq!(split_7z.part_index, 2);
        assert!(!split_7z.is_first);

        let rar1 = parse_archive_volume(Path::new("/dl/movie.part01.rar")).ok_or("part01")?;
        let rar2 = parse_archive_volume(Path::new("/dl/movie.part02.rar")).ok_or("part02")?;
        assert_eq!(rar1.group_key, rar2.group_key);
        assert!(rar1.is_first);
        assert!(!rar2.is_first);
        assert_eq!(rar1.archive_stem, "movie");
        Ok(())
    }

    #[tokio::test]
    async fn extracts_split_zip_and_cleans_up_parts() -> TestResult {
        let dir = tempfile::tempdir()?;
        let full_zip = dir.path().join("pack.zip");
        {
            let file = File::create(&full_zip)?;
            let mut zw = zip::ZipWriter::new(file);
            let opts = SimpleFileOptions::default();
            zw.start_file("hello/world.txt", opts)?;
            zw.write_all(b"split-zip-content")?;
            zw.finish()?;
        }

        let bytes = fs::read(&full_zip)?;
        fs::remove_file(&full_zip)?;
        let mid = bytes.len() / 2;
        let part1 = dir.path().join("pack.zip.001");
        let part2 = dir.path().join("pack.zip.002");
        fs::write(&part1, &bytes[..mid])?;
        fs::write(&part2, &bytes[mid..])?;

        let _outcome = extract_archive(
            &part2,
            ExtractOptions {
                extract_into_subfolder: true,
                delete_archives_after_extraction: true,
                passwords: Vec::new(),
                external_extractor_path: None,
            },
            &CancellationToken::new(),
        )
        .await?;

        assert_eq!(
            fs::read_to_string(dir.path().join("pack/hello/world.txt"))?,
            "split-zip-content"
        );
        assert!(!part1.exists());
        assert!(!part2.exists());
        Ok(())
    }

    #[tokio::test]
    async fn extracts_encrypted_7z_and_tar_gz_and_blocks_zip_slip() -> TestResult {
        let dir = tempfile::tempdir()?;
        // 1. Encrypted 7z
        let src_7z = dir.path().join("src7z");
        fs::create_dir_all(src_7z.join("docs"))?;
        fs::write(src_7z.join("docs/readme.txt"), b"top-secret")?;
        let archive_7z = dir.path().join("secret.7z");
        {
            let out = File::create(&archive_7z)?;
            sevenz_rust2::compress_encrypted(&src_7z, out, Password::from("s3cr3t"))?;
        }

        let err = extract_archive(
            &archive_7z,
            ExtractOptions {
                extract_into_subfolder: false,
                delete_archives_after_extraction: false,
                passwords: vec!["wrong".to_owned()],
                external_extractor_path: None,
            },
            &CancellationToken::new(),
        )
        .await
        .err()
        .ok_or("expected password error")?;
        assert!(matches!(err, ArchiveError::PasswordRequiredOrWrong));

        let ok = extract_archive(
            &archive_7z,
            ExtractOptions {
                extract_into_subfolder: true,
                delete_archives_after_extraction: false,
                passwords: vec!["wrong".to_owned(), "s3cr3t".to_owned()],
                external_extractor_path: None,
            },
            &CancellationToken::new(),
        )
        .await?;
        assert_eq!(ok.destination, dir.path().join("secret"));
        assert_eq!(
            fs::read_to_string(dir.path().join("secret/docs/readme.txt"))?,
            "top-secret"
        );

        // 2. tar.gz
        let tar_gz = dir.path().join("bundle.tar.gz");
        {
            let f = File::create(&tar_gz)?;
            let enc = GzEncoder::new(f, Compression::default());
            let mut tb = TarBuilder::new(enc);
            let data = b"tar-gz-payload";
            let mut hdr = tar::Header::new_gnu();
            hdr.set_size(data.len() as u64);
            hdr.set_mode(0o644);
            hdr.set_cksum();
            tb.append_data(&mut hdr, "inner/file.txt", &data[..])?;
            let enc = tb.into_inner()?;
            enc.finish()?;
        }
        let tar_out = extract_archive(
            &tar_gz,
            ExtractOptions {
                extract_into_subfolder: true,
                ..ExtractOptions::default()
            },
            &CancellationToken::new(),
        )
        .await?;
        assert_eq!(tar_out.destination, dir.path().join("bundle"));
        assert_eq!(
            fs::read_to_string(dir.path().join("bundle/inner/file.txt"))?,
            "tar-gz-payload"
        );

        // 3. Zip-Slip rejection
        let bad_zip = dir.path().join("evil.zip");
        {
            let f = File::create(&bad_zip)?;
            let mut zw = zip::ZipWriter::new(f);
            zw.start_file("../escape.txt", SimpleFileOptions::default())?;
            zw.write_all(b"pwned")?;
            zw.finish()?;
        }
        let slip_err = extract_archive(
            &bad_zip,
            ExtractOptions::default(),
            &CancellationToken::new(),
        )
        .await
        .err()
        .ok_or("expected UnsafeEntry")?;
        assert!(matches!(slip_err, ArchiveError::UnsafeEntry(_)));
        assert!(!dir.path().join("../escape.txt").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn extracts_rar_via_external_tool_stub() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir()?;
        let rar1 = dir.path().join("show.part1.rar");
        let rar2 = dir.path().join("show.part2.rar");
        fs::write(&rar1, b"Rar!part1")?;
        fs::write(&rar2, b"Rar!part2")?;

        let stub = dir.path().join("mock-7z");
        fs::write(
            &stub,
            "#!/bin/sh\nout=\"\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    -o*) out=\"${arg#-o}\" ;;\n  esac\ndone\nmkdir -p \"$out\"\necho \"extracted-from-rar\" > \"$out/episode.txt\"\nexit 0\n",
        )?;
        let mut perms = fs::metadata(&stub)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&stub, perms)?;

        let outcome = extract_archive(
            &rar2,
            ExtractOptions {
                extract_into_subfolder: true,
                delete_archives_after_extraction: true,
                passwords: Vec::new(),
                external_extractor_path: Some(stub),
            },
            &CancellationToken::new(),
        )
        .await?;

        assert_eq!(outcome.destination, dir.path().join("show"));
        assert_eq!(
            fs::read_to_string(dir.path().join("show/episode.txt"))?,
            "extracted-from-rar\n"
        );
        assert!(!rar1.exists());
        assert!(!rar2.exists());
        Ok(())
    }
}
