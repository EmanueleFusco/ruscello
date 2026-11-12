use std::{fs::File, io::Read, path::Path};

use ruscello_core::{FileType, SourceKind};

pub(super) fn detect(path: &Path, mime: Option<&str>, source: SourceKind) -> FileType {
    if source == SourceKind::Torrent {
        return FileType::Torrents;
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let from_extension = match extension.as_str() {
        "torrent" => Some(FileType::Torrents),
        "7z" | "rar" | "zip" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "tar" | "iso" => {
            Some(FileType::Archives)
        }
        "exe" | "msi" | "msix" | "appimage" | "app" | "apk" | "dmg" | "deb" | "rpm" | "pkg" => {
            Some(FileType::Programs)
        }
        _ => mime_guess::from_path(path)
            .first_raw()
            .and_then(classify_mime),
    };
    let from_header = mime.and_then(classify_mime);
    let mut prefix = Vec::new();
    let magic = File::open(path)
        .and_then(|file| file.take(8192).read_to_end(&mut prefix))
        .ok()
        .and_then(|_| infer::get(&prefix))
        .map(|kind| kind.mime_type());
    if magic == Some("application/zip")
        && (is_office_extension(&extension) || mime.is_some_and(is_office_mime))
    {
        return FileType::Documents;
    }
    magic
        .and_then(classify_mime)
        .or(from_header)
        .or(from_extension)
        .unwrap_or(FileType::Other)
}

fn is_office_extension(extension: &str) -> bool {
    matches!(extension, "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp")
}

fn is_office_mime(mime: &str) -> bool {
    mime.starts_with("application/vnd.openxmlformats-officedocument.")
        || mime.starts_with("application/vnd.oasis.opendocument.")
}

fn classify_mime(mime: &str) -> Option<FileType> {
    let mime = mime.split(';').next()?.trim().to_ascii_lowercase();
    match mime.as_str() {
        "application/x-bittorrent" => Some(FileType::Torrents),
        "application/pdf"
        | "application/rtf"
        | "application/msword"
        | "application/json"
        | "application/vnd.ms-excel"
        | "application/vnd.ms-powerpoint"
        | "application/epub+zip" => Some(FileType::Documents),
        "application/zip"
        | "application/gzip"
        | "application/x-gzip"
        | "application/x-7z-compressed"
        | "application/vnd.rar"
        | "application/x-rar-compressed"
        | "application/x-tar"
        | "application/x-bzip2"
        | "application/x-xz"
        | "application/zstd"
        | "application/x-iso9660-image" => Some(FileType::Archives),
        "application/x-executable"
        | "application/x-msdownload"
        | "application/vnd.microsoft.portable-executable"
        | "application/x-msi"
        | "application/x-apple-diskimage"
        | "application/x-mach-binary"
        | "application/vnd.android.package-archive"
        | "application/vnd.debian.binary-package"
        | "application/x-rpm"
        | "application/x-elf" => Some(FileType::Programs),
        _ if mime.starts_with("video/") => Some(FileType::Video),
        _ if mime.starts_with("audio/") => Some(FileType::Audio),
        _ if mime.starts_with("image/") => Some(FileType::Images),
        _ if mime.starts_with("text/") || is_office_mime(&mime) => Some(FileType::Documents),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io};

    #[test]
    fn detects_magic_then_mime_then_extension() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let wrong = temp.path().join("wrong.mp3");
        fs::write(&wrong, b"\x89PNG\r\n\x1a\n")?;
        assert_eq!(
            detect(&wrong, Some("video/mp4"), SourceKind::Http),
            FileType::Images
        );
        let unknown = temp.path().join("unknown.pdf");
        fs::write(&unknown, b"placeholder")?;
        assert_eq!(
            detect(&unknown, Some("Audio/FLAC; x=1"), SourceKind::Http),
            FileType::Audio
        );
        assert_eq!(
            detect(&unknown, Some("application/octet-stream"), SourceKind::Http),
            FileType::Documents
        );
        assert_eq!(
            detect(&unknown, None, SourceKind::Torrent),
            FileType::Torrents
        );
        Ok(())
    }
}
