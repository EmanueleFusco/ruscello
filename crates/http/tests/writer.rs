use std::{error::Error, fs};

use bytes::Bytes;
use ruscello_http::{FileWriter, WriterError, sanitize_filename};

type TestResult = Result<(), Box<dyn Error>>;

#[tokio::test]
async fn preallocates_and_coalesces_positional_writes_before_publication() -> TestResult {
    let directory = tempfile::tempdir()?;
    let mut writer = FileWriter::create(directory.path(), "résumé.bin", Some(12)).await?;
    let partial = writer.partial_path().to_owned();
    assert_eq!(fs::metadata(&partial)?.len(), 12);
    writer.write_at(6, Bytes::from_static(b"ghijkl")).await?;
    writer.write_at(0, Bytes::from_static(b"abc")).await?;
    writer.write_at(3, Bytes::from_static(b"def")).await?;
    writer.write_at(4, Bytes::from_static(b"EF")).await?;
    writer.sync_data().await?;
    assert_eq!(fs::read(&partial)?, b"abcdEFghijkl");

    let occupied = directory.path().join("résumé.bin");
    fs::write(&occupied, b"existing data")?;
    let result = writer.finish().await?;
    assert_eq!(
        result.file_name().and_then(|name| name.to_str()),
        Some("résumé (1).bin")
    );
    assert_eq!(fs::read(result)?, b"abcdEFghijkl");
    assert_eq!(fs::read(occupied)?, b"existing data");
    assert!(!partial.try_exists()?);
    Ok(())
}

#[tokio::test]
async fn refuses_to_publish_a_file_with_gaps_or_write_outside_its_size() -> TestResult {
    let directory = tempfile::tempdir()?;
    let mut writer = FileWriter::create(directory.path(), "gaps.dat", Some(8)).await?;
    assert!(matches!(
        writer.write_at(2, Bytes::from_static(b"partial")).await,
        Err(WriterError::Bounds)
    ));
    let mut writer = FileWriter::create(directory.path(), "gaps.dat", Some(8)).await?;
    writer.write_at(4, Bytes::from_static(b"part")).await?;
    let partial = writer.partial_path().to_owned();
    assert!(matches!(
        writer.finish().await,
        Err(WriterError::Incomplete)
    ));
    assert!(partial.try_exists()?);
    assert!(!directory.path().join("gaps.dat").try_exists()?);
    Ok(())
}

#[tokio::test]
async fn publishes_to_long_unicode_paths_and_accepts_unknown_lengths() -> TestResult {
    let directory = tempfile::tempdir()?;
    let nested = (0..8).fold(directory.path().to_owned(), |path, _| {
        path.join("a long directory component with a Unicode name ò")
    });
    fs::create_dir_all(&nested)?;
    let mut writer = FileWriter::create(&nested, "file?.txt", None).await?;
    writer.write_at(0, Bytes::from_static(b"hello")).await?;
    let result = writer.finish().await?;
    assert_eq!(
        result.file_name().and_then(|name| name.to_str()),
        Some("file_.txt")
    );
    assert_eq!(fs::read(result)?, b"hello");
    Ok(())
}

#[test]
fn sanitizes_reserved_names_and_truncates_on_unicode_boundaries() {
    assert_eq!(sanitize_filename("AUX.txt"), "_AUX.txt");
    assert_eq!(sanitize_filename("COM¹.txt"), "_COM¹.txt");
    assert_eq!(sanitize_filename("a/b:c\\d\u{0000} . "), "a_b_c_d_");
    assert_eq!(sanitize_filename("..."), "download");
    let long = sanitize_filename(&format!("{}.txt", "界".repeat(150)));
    assert!(long.len() <= 220);
    assert!(long.ends_with(".txt"));
}
