use std::{
    fs::File,
    io::{self, BufReader},
    path::{Component, Path, PathBuf},
};

use ruscello_http::{ChecksumAlgorithm, ExpectedChecksum};
use tokio_util::sync::CancellationToken;

const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidecarKind {
    Sfv,
    Md5,
    Sha256,
}

impl SidecarKind {
    pub(crate) fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?;
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".sfv") {
            return Some(Self::Sfv);
        }
        if lower.ends_with(".md5")
            || lower.ends_with(".md5sum")
            || lower == "md5sums"
            || lower == "md5sum.txt"
        {
            return Some(Self::Md5);
        }
        if lower.ends_with(".sha256")
            || lower.ends_with(".sha256sum")
            || lower == "sha256sums"
            || lower == "sha256sum.txt"
        {
            return Some(Self::Sha256);
        }
        None
    }

    pub(crate) const fn algorithm(self) -> ChecksumAlgorithm {
        match self {
            Self::Sfv => ChecksumAlgorithm::Crc32,
            Self::Md5 => ChecksumAlgorithm::Md5,
            Self::Sha256 => ChecksumAlgorithm::Sha256,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChecksumEntry {
    pub relative_path: PathBuf,
    pub expected: ExpectedChecksum,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedFile {
    pub file: PathBuf,
    pub checksum_file: PathBuf,
    pub algorithm: ChecksumAlgorithm,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ChecksumVerifyError {
    #[error("checksum mismatch for {file} (expected by {checksum_file})")]
    Mismatch {
        file: String,
        checksum_file: String,
        algorithm: ChecksumAlgorithm,
    },
    #[error("unsafe relative path in checksum manifest: {0}")]
    UnsafePath(String),
    #[error("checksum manifest too large: {0}")]
    ManifestTooLarge(String),
    #[error("checksum verification cancelled")]
    Cancelled,
    #[error("checksum I/O failed: {0}")]
    Io(#[from] io::Error),
}

pub(crate) fn is_checksum_file(path: &Path) -> bool {
    SidecarKind::from_path(path).is_some()
}

pub(crate) async fn verify_for_completed(
    completed_path: &Path,
    cancellation: &CancellationToken,
) -> Result<Vec<VerifiedFile>, ChecksumVerifyError> {
    let completed = completed_path.to_path_buf();
    let cancel = cancellation.clone();
    tokio::task::spawn_blocking(move || verify_blocking(&completed, &cancel))
        .await
        .map_err(|err| ChecksumVerifyError::Io(io::Error::other(err)))?
}

fn verify_blocking(
    completed_path: &Path,
    cancellation: &CancellationToken,
) -> Result<Vec<VerifiedFile>, ChecksumVerifyError> {
    if cancellation.is_cancelled() {
        return Err(ChecksumVerifyError::Cancelled);
    }

    let Some(parent) = completed_path.parent() else {
        return Ok(Vec::new());
    };

    if SidecarKind::from_path(completed_path).is_some() {
        return verify_manifest_targets(completed_path, parent, None, cancellation);
    }

    if !completed_path.is_file() {
        return Ok(Vec::new());
    }

    let Ok(read_dir) = std::fs::read_dir(parent) else {
        return Ok(Vec::new());
    };

    let mut manifests: Vec<PathBuf> = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && SidecarKind::from_path(path).is_some())
        .collect();
    manifests.sort();

    let mut verified = Vec::new();
    for manifest in manifests {
        let mut results =
            verify_manifest_targets(&manifest, parent, Some(completed_path), cancellation)?;
        verified.append(&mut results);
    }
    Ok(verified)
}

fn verify_manifest_targets(
    manifest_path: &Path,
    base_dir: &Path,
    only_target: Option<&Path>,
    cancellation: &CancellationToken,
) -> Result<Vec<VerifiedFile>, ChecksumVerifyError> {
    let Some(kind) = SidecarKind::from_path(manifest_path) else {
        return Ok(Vec::new());
    };
    let entries = parse_manifest_file(manifest_path, kind)?;
    let only_canonical = only_target.and_then(|path| std::fs::canonicalize(path).ok());

    let mut verified = Vec::new();
    for entry in entries {
        if cancellation.is_cancelled() {
            return Err(ChecksumVerifyError::Cancelled);
        }
        let target_path = base_dir.join(&entry.relative_path);
        if !target_path.is_file() {
            continue;
        }
        if let Some(expected_target) = &only_canonical {
            let Ok(candidate_canonical) = std::fs::canonicalize(&target_path) else {
                continue;
            };
            if &candidate_canonical != expected_target {
                continue;
            }
        } else if is_partial_temp(&target_path) {
            continue;
        }

        let file = File::open(&target_path)?;
        let reader = BufReader::new(file);
        let ok = entry.expected.verify(reader, cancellation).map_err(|err| {
            if err.kind() == io::ErrorKind::Interrupted {
                ChecksumVerifyError::Cancelled
            } else {
                ChecksumVerifyError::Io(err)
            }
        })?;
        if !ok {
            return Err(ChecksumVerifyError::Mismatch {
                file: target_path.display().to_string(),
                checksum_file: manifest_path.display().to_string(),
                algorithm: kind.algorithm(),
            });
        }
        verified.push(VerifiedFile {
            file: target_path,
            checksum_file: manifest_path.to_path_buf(),
            algorithm: kind.algorithm(),
        });
    }

    Ok(verified)
}

fn is_partial_temp(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".rscpart") || name.ends_with(".rscpart.ctl"))
}

pub(crate) fn parse_manifest_file(
    path: &Path,
    kind: SidecarKind,
) -> Result<Vec<ChecksumEntry>, ChecksumVerifyError> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > MAX_MANIFEST_BYTES {
        return Err(ChecksumVerifyError::ManifestTooLarge(
            path.display().to_string(),
        ));
    }
    let text = std::fs::read_to_string(path)?;
    parse_manifest_text(&text, kind, infer_sidecar_target(path))
}

fn infer_sidecar_target(manifest_path: &Path) -> Option<String> {
    let file_name = manifest_path.file_name()?.to_str()?;
    let lower = file_name.to_ascii_lowercase();
    for suffix in [".sfv", ".md5", ".md5sum", ".sha256", ".sha256sum"] {
        if lower.ends_with(suffix) && file_name.len() > suffix.len() {
            return Some(file_name[..file_name.len() - suffix.len()].to_owned());
        }
    }
    None
}

pub(crate) fn parse_manifest_text(
    text: &str,
    kind: SidecarKind,
    default_target: Option<String>,
) -> Result<Vec<ChecksumEntry>, ChecksumVerifyError> {
    match kind {
        SidecarKind::Sfv => parse_sfv(text),
        SidecarKind::Md5 => parse_digest(text, ChecksumAlgorithm::Md5, default_target.as_deref()),
        SidecarKind::Sha256 => {
            parse_digest(text, ChecksumAlgorithm::Sha256, default_target.as_deref())
        }
    }
}

fn parse_sfv(text: &str) -> Result<Vec<ChecksumEntry>, ChecksumVerifyError> {
    let mut entries = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        let Some((name_part, crc_part)) = line.rsplit_once(|ch: char| ch.is_ascii_whitespace())
        else {
            continue;
        };
        let filename = name_part.trim();
        let hex = crc_part.trim();
        let Ok(expected) = ExpectedChecksum::from_hex(ChecksumAlgorithm::Crc32, hex) else {
            continue;
        };
        let Some(relative_path) = safe_relative_path(filename) else {
            return Err(ChecksumVerifyError::UnsafePath(filename.to_owned()));
        };
        entries.push(ChecksumEntry {
            relative_path,
            expected,
        });
    }
    Ok(entries)
}

fn parse_digest(
    text: &str,
    algorithm: ChecksumAlgorithm,
    default_target: Option<&str>,
) -> Result<Vec<ChecksumEntry>, ChecksumVerifyError> {
    let mut entries = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        // BSD style: `SHA256 (file name.zip) = <hex>` or `MD5 (file.zip) = <hex>`
        if let Some((raw_target, hex)) = parse_bsd_digest_line(line, algorithm)
            && let Ok(expected) = ExpectedChecksum::from_hex(algorithm, hex)
        {
            let Some(relative_path) = safe_relative_path(raw_target) else {
                return Err(ChecksumVerifyError::UnsafePath(raw_target.to_owned()));
            };
            entries.push(ChecksumEntry {
                relative_path,
                expected,
            });
            continue;
        }

        // Bare hash line in a single-file sidecar (e.g. `archive.zip.sha256` containing only `<hex>`)
        if let Some(target_name) = default_target
            && !line.contains(char::is_whitespace)
            && let Ok(expected) = ExpectedChecksum::from_hex(algorithm, line)
        {
            let Some(relative_path) = safe_relative_path(target_name) else {
                return Err(ChecksumVerifyError::UnsafePath(target_name.to_owned()));
            };
            entries.push(ChecksumEntry {
                relative_path,
                expected,
            });
            continue;
        }

        // GNU coreutils style: `<hex>  <filename>` or `<hex> *<filename>`
        let Some((hex, rest)) = line.split_once(|ch: char| ch.is_ascii_whitespace()) else {
            continue;
        };
        let Ok(expected) = ExpectedChecksum::from_hex(algorithm, hex.trim()) else {
            continue;
        };
        let raw_target = rest
            .trim_start_matches([' ', '\t'])
            .strip_prefix('*')
            .unwrap_or_else(|| rest.trim_start_matches([' ', '\t']));
        if raw_target.is_empty() {
            continue;
        }
        let Some(relative_path) = safe_relative_path(raw_target) else {
            return Err(ChecksumVerifyError::UnsafePath(raw_target.to_owned()));
        };
        entries.push(ChecksumEntry {
            relative_path,
            expected,
        });
    }
    Ok(entries)
}

fn parse_bsd_digest_line(line: &str, algorithm: ChecksumAlgorithm) -> Option<(&str, &str)> {
    let prefixes: &[&str] = match algorithm {
        ChecksumAlgorithm::Md5 => &["MD5 (", "md5 ("],
        ChecksumAlgorithm::Sha256 => &["SHA256 (", "SHA2-256 (", "sha256 (", "sha2-256 ("],
        ChecksumAlgorithm::Sha1 | ChecksumAlgorithm::Crc32 => return None,
    };
    let rest = prefixes
        .iter()
        .find_map(|prefix| line.strip_prefix(prefix))?;
    let (filename, hex) = rest.rsplit_once(") = ")?;
    Some((filename, hex.trim()))
}

pub(crate) fn safe_relative_path(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains('\0') {
        return None;
    }
    // Reject absolute Windows paths (`C:\...`), UNC paths, or leading slashes.
    let normalized = trimmed.replace('\\', "/");
    if normalized.starts_with('/') {
        return None;
    }
    if normalized.as_bytes().get(1) == Some(&b':')
        && normalized
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
    {
        return None;
    }
    let mut clean = PathBuf::new();
    for component in Path::new(&normalized).components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!clean.as_os_str().is_empty()).then_some(clean)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn verifies_sfv_md5_and_sha256_valid_and_detects_corruption() -> TestResult {
        let dir = tempfile::tempdir()?;
        let payload = dir.path().join("track 01.bin");
        let bytes = b"ruscello-checksum-payload";
        std::fs::write(&payload, bytes)?;

        let crc = crc32fast::hash(bytes);
        let sfv = dir.path().join("album.sfv");
        std::fs::write(&sfv, format!("; comment\ntrack 01.bin {:08x}\n", crc))?;

        let md5_hex = {
            use md5::Digest as _;
            hex::encode(md5::Md5::digest(bytes))
        };
        let md5_file = dir.path().join("track 01.bin.md5");
        std::fs::write(&md5_file, format!("{md5_hex} *track 01.bin\n"))?;

        let sha_hex = {
            use sha2::Digest as _;
            hex::encode(sha2::Sha256::digest(bytes))
        };
        let sha_file = dir.path().join("track 01.bin.sha256");
        std::fs::write(&sha_file, format!("{sha_hex}\n"))?;

        let cancel = CancellationToken::new();
        let results = verify_for_completed(&payload, &cancel).await?;
        assert_eq!(results.len(), 3);

        // Also check BSD tag format in SHA256SUMS after corrupting payload
        std::fs::write(&payload, b"tampered")?;
        std::fs::write(&sha_file, format!("SHA256 (track 01.bin) = {sha_hex}\n"))?;
        let err = verify_for_completed(&payload, &cancel)
            .await
            .err()
            .ok_or("expected mismatch")?;
        assert!(matches!(err, ChecksumVerifyError::Mismatch { .. }));
        Ok(())
    }

    #[tokio::test]
    async fn rejects_path_traversal_entries() -> TestResult {
        let dir = tempfile::tempdir()?;
        let payload = dir.path().join("safe.bin");
        std::fs::write(&payload, b"ok")?;
        let manifest = dir.path().join("bad.sha256");
        std::fs::write(
            &manifest,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  ../escape.bin\n",
        )?;

        let err = verify_for_completed(&payload, &CancellationToken::new())
            .await
            .err()
            .ok_or("expected unsafe path error")?;
        assert!(matches!(err, ChecksumVerifyError::UnsafePath(_)));
        Ok(())
    }
}
