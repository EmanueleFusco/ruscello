use std::{collections::HashSet, path::Path, sync::LazyLock};

use aes::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::NoPadding};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use regex::Regex;
use reqwest::Url;
use serde::Deserialize;
use tokio::io::AsyncReadExt as _;

use crate::{ResolveError, http};

const LIMIT: usize = 8 * 1024 * 1024;
const TAIL: usize = 88;
const SERVICE_KEY: &[u8; 16] = b"cb99b5cbc24db398";
const SERVICE_IV: &[u8; 16] = b"9bc24cb995cb8db3";
static LINK: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"(?i)(?:https?://|magnet:\?)[^\s<>"']+"#).ok());

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedLink {
    pub url: Url,
    pub filename: Option<String>,
    pub size: Option<u64>,
    pub mirrors: Vec<Url>,
    pub checksum: Option<String>,
}

impl ImportedLink {
    fn new(url: Url) -> Self {
        Self {
            url,
            filename: None,
            size: None,
            mirrors: Vec::new(),
            checksum: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedPackage {
    pub name: String,
    pub links: Vec<ImportedLink>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerFile {
    Packages(Vec<ImportedPackage>),
    Torrent,
}

pub fn links(input: &str) -> Vec<Url> {
    let Some(regex) = LINK.as_ref() else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    regex
        .find_iter(input)
        .filter_map(|matched| {
            let raw = matched
                .as_str()
                .trim_end_matches(['.', ',', ';', '!', ']', '}']);
            let raw = if raw.ends_with(')') && raw.matches(')').count() > raw.matches('(').count() {
                raw.strip_suffix(')').unwrap_or(raw)
            } else {
                raw
            };
            parse_link(raw)
        })
        .filter(|url| seen.insert(url.clone()))
        .collect()
}

fn parse_link(value: &str) -> Option<Url> {
    let mut url = Url::parse(value.trim()).ok()?;
    if url.scheme() == "magnet" {
        return url
            .query_pairs()
            .any(|(name, _)| name == "xt")
            .then_some(url);
    }
    http::validate(&url).ok()?;
    url.set_fragment(None);
    Some(url)
}

#[derive(Debug, Clone)]
pub struct DlcKeyService {
    primary: Url,
    fallback: Option<Url>,
}

impl DlcKeyService {
    pub fn new(primary: Url, fallback: Option<Url>) -> Self {
        Self { primary, fallback }
    }

    pub fn production(allow_fallback: bool) -> Result<Self, ResolveError> {
        let primary = Url::parse("https://service.jdownloader.org/dlcrypt/service.php")
            .map_err(|_| ResolveError::KeyService)?;
        let fallback = allow_fallback
            .then(|| Url::parse("https://dcrypt.it/decrypt/paste"))
            .transpose()
            .map_err(|_| ResolveError::KeyService)?;
        Ok(Self::new(primary, fallback))
    }

    pub async fn decrypt(&self, raw: &str) -> Result<Vec<ImportedPackage>, ResolveError> {
        let raw = raw.trim();
        if raw.len() <= TAIL || raw.len() > LIMIT || !raw.is_ascii() {
            return Err(ResolveError::InvalidContainer);
        }
        let (body, tail) = raw.split_at(raw.len() - TAIL);
        let encrypted = STANDARD
            .decode(body)
            .map_err(|_| ResolveError::InvalidContainer)?;
        if encrypted.is_empty() || encrypted.len() % 16 != 0 || STANDARD.decode(tail).is_err() {
            return Err(ResolveError::InvalidContainer);
        }
        // The legacy key exchange receives only the encrypted key tail.
        let client = http::client()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let response = client
            .get(self.primary.clone())
            .query(&[("srcType", "dlc"), ("destType", "pylo"), ("data", tail)])
            .send()
            .await;
        if let Ok(response) = response
            && let Ok(xml) = http::body(response, 8192).await
            && let Some(rc) = xml
                .split_once("<rc>")
                .and_then(|(_, rest)| rest.split_once("</rc>"))
                .map(|(rc, _)| rc)
        {
            return decrypt_payload(encrypted, rc);
        }
        let fallback = self.fallback.as_ref().ok_or(ResolveError::KeyService)?;
        let response = client
            .post(fallback.clone())
            .form(&[("content", raw)])
            .send()
            .await
            .map_err(|_| ResolveError::KeyService)?;
        let body = http::body(response, LIMIT)
            .await
            .map_err(|_| ResolveError::KeyService)?;
        let data: Fallback = serde_json::from_str(&body).map_err(|_| ResolveError::KeyService)?;
        let links = data
            .success
            .links
            .iter()
            .filter_map(|link| parse_link(link))
            .map(ImportedLink::new)
            .collect();
        finish(vec![ImportedPackage {
            name: String::new(),
            links,
        }])
    }
}

#[derive(Deserialize)]
struct Fallback {
    success: FallbackLinks,
}

#[derive(Deserialize)]
struct FallbackLinks {
    links: Vec<String>,
}

fn decrypt_payload(mut encrypted: Vec<u8>, rc: &str) -> Result<Vec<ImportedPackage>, ResolveError> {
    type Decryptor = cbc::Decryptor<aes::Aes128>;
    let mut wrapped = STANDARD
        .decode(rc.trim())
        .map_err(|_| ResolveError::KeyService)?;
    let key = Decryptor::new(SERVICE_KEY.into(), SERVICE_IV.into())
        .decrypt_padded::<NoPadding>(&mut wrapped)
        .map_err(|_| ResolveError::KeyService)?;
    let key: &[u8; 16] = key
        .get(..16)
        .and_then(|key| key.try_into().ok())
        .ok_or(ResolveError::KeyService)?;
    let plaintext = Decryptor::new(key.into(), key.into())
        .decrypt_padded::<NoPadding>(&mut encrypted)
        .map_err(|_| ResolveError::InvalidContainer)?;
    let plaintext = std::str::from_utf8(plaintext).map_err(|_| ResolveError::InvalidContainer)?;
    let xml = decode(plaintext.trim_matches(|ch: char| ch == '\0' || ch.is_ascii_whitespace()))?;
    let root: Dlc = quick_xml::de::from_str(&xml).map_err(|_| ResolveError::InvalidContainer)?;
    let mut packages = Vec::new();
    for package in root.content.packages {
        let mut links = Vec::new();
        for file in package.files {
            if let Some(url) = parse_link(&decode(&file.url)?) {
                links.push(ImportedLink {
                    url,
                    filename: file
                        .filename
                        .as_deref()
                        .map(decode)
                        .transpose()?
                        .filter(|name| !name.is_empty()),
                    size: file
                        .size
                        .as_deref()
                        .map(decode)
                        .transpose()?
                        .and_then(|size| size.parse().ok()),
                    mirrors: Vec::new(),
                    checksum: None,
                });
            }
        }
        packages.push(ImportedPackage {
            name: decode(&package.name)?,
            links,
        });
    }
    finish(packages)
}

fn decode(encoded: &str) -> Result<String, ResolveError> {
    let bytes = STANDARD
        .decode(encoded.trim())
        .map_err(|_| ResolveError::InvalidContainer)?;
    String::from_utf8(bytes).map_err(|_| ResolveError::InvalidContainer)
}

#[derive(Deserialize)]
struct Dlc {
    content: DlcContent,
}

#[derive(Deserialize)]
struct DlcContent {
    #[serde(rename = "package", default)]
    packages: Vec<DlcPackage>,
}

#[derive(Deserialize)]
struct DlcPackage {
    #[serde(rename = "@name", default)]
    name: String,
    #[serde(rename = "file", default)]
    files: Vec<DlcFile>,
}

#[derive(Deserialize)]
struct DlcFile {
    url: String,
    filename: Option<String>,
    size: Option<String>,
}

#[derive(Deserialize)]
struct Metalink {
    #[serde(rename = "file", default)]
    files: Vec<MetalinkFile>,
    #[serde(rename = "files")]
    files_group: Option<MetalinkGroup>,
}

#[derive(Deserialize)]
struct MetalinkGroup {
    #[serde(rename = "file", default)]
    files: Vec<MetalinkFile>,
}

#[derive(Deserialize)]
struct MetalinkFile {
    #[serde(rename = "@name")]
    name: String,
    size: Option<u64>,
    #[serde(rename = "url", default)]
    urls: Vec<String>,
    resources: Option<MetalinkResources>,
    #[serde(rename = "hash", default)]
    hashes: Vec<MetalinkHash>,
}

#[derive(Deserialize)]
struct MetalinkHash {
    #[serde(rename = "@type")]
    algorithm: String,
    #[serde(rename = "$text")]
    value: String,
}

#[derive(Deserialize)]
struct MetalinkResources {
    #[serde(rename = "url", default)]
    urls: Vec<String>,
}

pub async fn import_file(path: &Path, dlc: &DlcKeyService) -> Result<ContainerFile, ResolveError> {
    let ext = path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if ext == "torrent" {
        return Ok(ContainerFile::Torrent);
    }
    let mut raw = String::new();
    tokio::fs::File::open(path)
        .await?
        .take((LIMIT + 1) as u64)
        .read_to_string(&mut raw)
        .await?;
    if raw.len() > LIMIT {
        return Err(ResolveError::TooLarge);
    }
    let raw = raw.trim_start_matches('\u{feff}');
    let name = path
        .file_stem()
        .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    let packages = match ext.as_str() {
        "dlc" => {
            let mut packages = dlc.decrypt(raw).await?;
            for package in &mut packages {
                if package.name.is_empty() {
                    package.name.clone_from(&name);
                }
            }
            packages
        }
        "txt" | "m3u" | "m3u8" => vec![ImportedPackage {
            name,
            links: links(raw).into_iter().map(ImportedLink::new).collect(),
        }],
        "metalink" | "meta4" => vec![ImportedPackage {
            name,
            links: metalink(raw)?,
        }],
        _ => return Err(ResolveError::InvalidContainer),
    };
    finish(packages).map(ContainerFile::Packages)
}

fn metalink(raw: &str) -> Result<Vec<ImportedLink>, ResolveError> {
    let root: Metalink =
        quick_xml::de::from_str(raw).map_err(|_| ResolveError::InvalidContainer)?;
    Ok(root
        .files
        .into_iter()
        .chain(root.files_group.into_iter().flat_map(|group| group.files))
        .filter_map(|file| {
            let mut urls = file
                .urls
                .iter()
                .chain(file.resources.iter().flat_map(|r| &r.urls))
                .filter_map(|url| parse_link(url));
            let url = urls.next()?;
            Some(ImportedLink {
                url,
                filename: Some(file.name),
                size: file.size,
                checksum: metalink_checksum(&file.hashes),
                mirrors: {
                    let mut unique = HashSet::new();
                    urls.filter(|mirror| unique.insert(mirror.clone()))
                        .collect()
                },
            })
        })
        .collect())
}

fn metalink_checksum(hashes: &[MetalinkHash]) -> Option<String> {
    [
        ("sha-256", "sha256", 64),
        ("sha-1", "sha1", 40),
        ("md5", "md5", 32),
    ]
    .into_iter()
    .find_map(|(alias, algorithm, size)| {
        let hash = hashes.iter().find(|hash| {
            (hash.algorithm.eq_ignore_ascii_case(alias)
                || hash.algorithm.eq_ignore_ascii_case(algorithm))
                && hash.value.trim().len() == size
                && hash
                    .value
                    .trim()
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
        })?;
        Some(format!("{algorithm}:{}", hash.value.trim()))
    })
}

fn finish(mut packages: Vec<ImportedPackage>) -> Result<Vec<ImportedPackage>, ResolveError> {
    let mut seen = HashSet::new();
    for package in &mut packages {
        package.links.retain(|link| seen.insert(link.url.clone()));
    }
    packages.retain(|package| !package.links.is_empty());
    if packages.is_empty() {
        Err(ResolveError::NoDownload)
    } else {
        Ok(packages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockModeEncrypt as _;
    use ruscello_testkit::{Reply, StatusCode, TestServer};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn encrypted_fixture() -> Result<(String, String, String), ResolveError> {
        fn encrypt(input: &str, key: &[u8; 16], iv: &[u8; 16]) -> Result<String, ResolveError> {
            let mut bytes = input.as_bytes().to_vec();
            bytes.resize(bytes.len().next_multiple_of(16), 0);
            let length = bytes.len();
            let output = cbc::Encryptor::<aes::Aes128>::new(key.into(), iv.into())
                .encrypt_padded::<NoPadding>(&mut bytes, length)
                .map_err(|_| ResolveError::InvalidContainer)?;
            Ok(STANDARD.encode(output))
        }
        let key = b"test key 16 byte";
        let plaintext = STANDARD.encode(include_str!("../tests/fixtures/dlc-2026-11-14.xml"));
        let encrypted = encrypt(&plaintext, key, key)?;
        let tail = STANDARD.encode([1_u8; 64]);
        let rc = encrypt(
            std::str::from_utf8(key).map_err(|_| ResolveError::InvalidContainer)?,
            SERVICE_KEY,
            SERVICE_IV,
        )?;
        Ok((format!("{encrypted}{tail}"), tail, rc))
    }

    #[tokio::test]
    async fn decrypts_native_dlc_after_sending_only_the_key_tail() -> TestResult {
        let server = TestServer::start().await?;
        let (raw, tail, rc) = encrypted_fixture()?;
        let endpoint = server.route("/key", move |request| {
            let expected = format!(
                "srcType=dlc&destType=pylo&data={}",
                tail.replace('=', "%3D")
            );
            if request.uri.query() == Some(expected.as_str()) && request.body.is_empty() {
                Reply::html(format!("<response><rc>{rc}</rc></response>"))
            } else {
                Reply::status(StatusCode::BAD_REQUEST)
            }
        });
        let service = DlcKeyService::new(Url::parse(&endpoint)?, None);
        let folder = tempfile::tempdir()?;
        let file = folder.path().join("backup.dlc");
        tokio::fs::write(&file, format!("\n{raw}\n")).await?;
        let ContainerFile::Packages(packages) = import_file(&file, &service).await? else {
            return Err("expected packages".into());
        };
        assert_eq!(packages[0].name, "Test package");
        assert_eq!(
            packages[0].links[0].url.as_str(),
            "https://example.test/Sample.zip"
        );
        assert_eq!(packages[0].links[0].filename.as_deref(), Some("Sample.zip"));
        assert_eq!(packages[0].links[0].size, Some(42));
        assert!(matches!(
            service.decrypt("bogus").await,
            Err(ResolveError::InvalidContainer)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn fallback_and_metalink_import_are_deduplicated() -> TestResult {
        let server = TestServer::start().await?;
        let (raw, _, _) = encrypted_fixture()?;
        let primary =
            Url::parse(&server.route("/unavailable", |_| Reply::status(StatusCode::NOT_FOUND)))?;
        let fallback = Url::parse(&server.route("/fallback", |request| {
            if request.method == ruscello_testkit::Method::POST && request.body.starts_with(b"content=") {
                Reply::html(r#"{"success":{"links":["https://example.test/file","https://example.test/file#duplicate"]}}"#)
            } else { Reply::status(StatusCode::BAD_REQUEST) }
        }))?;
        assert!(matches!(
            DlcKeyService::new(primary.clone(), None)
                .decrypt(&raw)
                .await,
            Err(ResolveError::KeyService)
        ));
        let service = DlcKeyService::new(primary, Some(fallback));
        assert_eq!(service.decrypt(&raw).await?[0].links.len(), 1);
        let folder = tempfile::tempdir()?;
        let file = folder.path().join("fixture.meta4");
        tokio::fs::write(&file, r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="file.jar"><size>24</size>
        <url>https://example.test/file.jar</url><url>https://mirror.test/file.jar</url>
        </file></metalink>"#).await?;
        let ContainerFile::Packages(packages) = import_file(&file, &service).await? else {
            return Err("expected packages".into());
        };
        assert_eq!(packages[0].links[0].filename.as_deref(), Some("file.jar"));
        assert_eq!(packages[0].links[0].mirrors.len(), 1);
        Ok(())
    }
}
