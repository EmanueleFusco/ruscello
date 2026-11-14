use std::{
    collections::{HashMap, VecDeque},
    fs,
    net::IpAddr,
    path::Path,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::error::ApiError;

const PBKDF2_ITERATIONS: u32 = 100_000;
const SALT_BYTES: usize = 16;
const HASH_PREFIX: &str = "pbkdf2-sha256";
const MAX_SESSIONS: usize = 128;
const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const RATE_WINDOW: Duration = Duration::from_secs(60);
const MAX_FAILURES_PER_WINDOW: usize = 5;
const MAX_TRACKED_IPS: usize = 1024;

pub(crate) const SESSION_COOKIE_NAME: &str = "ruscello_session";

/// Hashes a plaintext password using PBKDF2-HMAC-SHA256 with a fresh random 16-byte salt.
pub fn hash_password(password: &str) -> Result<String, ApiError> {
    if password.is_empty() {
        return Err(ApiError::BadRequest(
            "password must not be empty".to_owned(),
        ));
    }
    let mut salt = [0_u8; SALT_BYTES];
    getrandom::fill(&mut salt).map_err(|error| ApiError::Random(error.to_string()))?;
    let derived = pbkdf2_hmac_sha256(password.as_bytes(), &salt, PBKDF2_ITERATIONS);
    Ok(format!(
        "{HASH_PREFIX}${PBKDF2_ITERATIONS}${}${}",
        hex::encode(salt),
        hex::encode(derived)
    ))
}

/// Verifies a plaintext password against a stored `pbkdf2-sha256$...` digest in constant time.
pub fn verify_password(password: &str, stored: &str) -> bool {
    let mut parts = stored.split('$');
    let Some(HASH_PREFIX) = parts.next() else {
        return false;
    };
    let Some(iters_str) = parts.next() else {
        return false;
    };
    let Some(salt_hex) = parts.next() else {
        return false;
    };
    let Some(hash_hex) = parts.next() else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    let Ok(iterations) = iters_str.parse::<u32>() else {
        return false;
    };
    if !(10_000..=2_000_000).contains(&iterations) {
        return false;
    }
    let Ok(salt) = hex::decode(salt_hex) else {
        return false;
    };
    let Ok(expected) = hex::decode(hash_hex) else {
        return false;
    };
    if expected.len() != 32 {
        return false;
    }
    let actual = pbkdf2_hmac_sha256(password.as_bytes(), &salt, iterations);
    constant_time_eq(&actual, &expected)
}

/// Async wrapper around [`hash_password`] using `spawn_blocking`.
pub async fn hash_password_async(password: String) -> Result<String, ApiError> {
    tokio::task::spawn_blocking(move || hash_password(&password)).await?
}

/// Async wrapper around [`verify_password`] using `spawn_blocking`.
pub async fn verify_password_async(password: String, stored: String) -> bool {
    tokio::task::spawn_blocking(move || verify_password(&password, &stored))
        .await
        .unwrap_or(false)
}

pub(crate) fn random_token_hex(num_bytes: usize) -> Result<String, ApiError> {
    let mut buf = vec![0_u8; num_bytes];
    getrandom::fill(&mut buf).map_err(|error| ApiError::Random(error.to_string()))?;
    Ok(hex::encode(buf))
}

pub(crate) fn random_pin_6() -> Result<String, ApiError> {
    let mut buf = [0_u8; 4];
    getrandom::fill(&mut buf).map_err(|error| ApiError::Random(error.to_string()))?;
    let val = u32::from_le_bytes(buf) % 1_000_000;
    Ok(format!("{val:06}"))
}

pub(crate) fn sha256_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    hex::encode(digest)
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;
    let mut key_block = [0_u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        let hashed = Sha256::digest(key);
        key_block[..32].copy_from_slice(&hashed);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36_u8; BLOCK_SIZE];
    let mut opad = [0x5c_u8; BLOCK_SIZE];
    for idx in 0..BLOCK_SIZE {
        ipad[idx] ^= key_block[idx];
        opad[idx] ^= key_block[idx];
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(data);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_digest);
    outer.finalize().into()
}

fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut msg = Vec::with_capacity(salt.len() + 4);
    msg.extend_from_slice(salt);
    msg.extend_from_slice(&1_u32.to_be_bytes());
    let mut u = hmac_sha256(password, &msg);
    let mut result = u;
    for _ in 1..iterations {
        u = hmac_sha256(password, &u);
        for (acc, byte) in result.iter_mut().zip(u.iter()) {
            *acc ^= *byte;
        }
    }
    result
}

/// Validates whether an `Origin` header belongs to a Chrome or Firefox WebExtension.
pub(crate) fn is_allowed_extension_origin(origin: &str) -> bool {
    let rest = if let Some(rest) = origin.strip_prefix("chrome-extension://") {
        rest
    } else if let Some(rest) = origin.strip_prefix("moz-extension://") {
        rest
    } else {
        return false;
    };
    !rest.is_empty()
        && !rest.contains('/')
        && !rest.contains('?')
        && !rest.contains('#')
        && !rest.chars().any(char::is_whitespace)
}

#[derive(Debug, Default)]
pub(crate) struct RateLimiter {
    attempts: HashMap<IpAddr, VecDeque<Instant>>,
}

impl RateLimiter {
    pub(crate) fn check(&mut self, ip: IpAddr, now: Instant) -> Result<(), u64> {
        self.prune_expired(now);
        let Some(entries) = self.attempts.get(&ip) else {
            return Ok(());
        };
        if entries.len() < MAX_FAILURES_PER_WINDOW {
            return Ok(());
        }
        let oldest = entries.front().copied().unwrap_or(now);
        let remaining = RATE_WINDOW.saturating_sub(now.saturating_duration_since(oldest));
        Err(remaining.as_secs().max(1))
    }

    pub(crate) fn record_failure(&mut self, ip: IpAddr, now: Instant) {
        self.prune_expired(now);
        if self.attempts.len() >= MAX_TRACKED_IPS && !self.attempts.contains_key(&ip) {
            self.attempts.clear();
        }
        let queue = self.attempts.entry(ip).or_default();
        queue.push_back(now);
        while queue.len() > MAX_FAILURES_PER_WINDOW {
            queue.pop_front();
        }
    }

    pub(crate) fn reset(&mut self, ip: IpAddr) {
        self.attempts.remove(&ip);
    }

    fn prune_expired(&mut self, now: Instant) {
        self.attempts.retain(|_, queue| {
            while queue
                .front()
                .is_some_and(|at| now.saturating_duration_since(*at) >= RATE_WINDOW)
            {
                queue.pop_front();
            }
            !queue.is_empty()
        });
    }
}

#[derive(Debug, Default)]
pub(crate) struct SessionStore {
    sessions: HashMap<String, Instant>,
}

impl SessionStore {
    pub(crate) fn insert(&mut self, token: String, now: Instant) {
        self.prune(now);
        if self.sessions.len() >= MAX_SESSIONS
            && let Some(oldest_key) = self
                .sessions
                .iter()
                .min_by_key(|(_, exp)| **exp)
                .map(|(k, _)| k.clone())
        {
            self.sessions.remove(&oldest_key);
        }
        self.sessions.insert(token, now + SESSION_TTL);
    }

    pub(crate) fn is_valid(&mut self, token: &str, now: Instant) -> bool {
        self.prune(now);
        let Some(expiry) = self.sessions.get_mut(token) else {
            return false;
        };
        if *expiry <= now {
            self.sessions.remove(token);
            return false;
        }
        *expiry = now + SESSION_TTL;
        true
    }

    pub(crate) fn remove(&mut self, token: &str) {
        self.sessions.remove(token);
    }

    pub(crate) fn clear(&mut self) {
        self.sessions.clear();
    }

    fn prune(&mut self, now: Instant) {
        self.sessions.retain(|_, exp| *exp > now);
    }
}

/// Discovered local loopback API metadata saved in `<data_dir>/local-api.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalApiEndpoint {
    pub port: u16,
    pub pid: u32,
    pub token: String,
}

const LOCAL_ENDPOINT_FILE: &str = "local-api.json";
const EXTENSION_TOKENS_FILE: &str = "extension-tokens.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct ExtensionTokensFile {
    token_hashes: Vec<String>,
}

/// Reads the local loopback API discovery file from `data_dir` (for CLI / same-user tooling).
pub fn discover_local_endpoint(data_dir: &Path) -> Option<LocalApiEndpoint> {
    let bytes = fs::read(data_dir.join(LOCAL_ENDPOINT_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn write_local_endpoint(
    data_dir: &Path,
    endpoint: &LocalApiEndpoint,
) -> Result<(), ApiError> {
    fs::create_dir_all(data_dir)?;
    let path = data_dir.join(LOCAL_ENDPOINT_FILE);
    let data = serde_json::to_vec_pretty(endpoint)?;
    write_private_file(&path, &data)?;
    Ok(())
}

pub(crate) fn remove_local_endpoint(data_dir: &Path) {
    let _ = fs::remove_file(data_dir.join(LOCAL_ENDPOINT_FILE));
}

pub(crate) fn load_extension_token_hashes(data_dir: &Path) -> Vec<String> {
    let Ok(bytes) = fs::read(data_dir.join(EXTENSION_TOKENS_FILE)) else {
        return Vec::new();
    };
    serde_json::from_slice::<ExtensionTokensFile>(&bytes)
        .map(|file| file.token_hashes)
        .unwrap_or_default()
}

pub(crate) fn save_extension_token_hashes(
    data_dir: &Path,
    hashes: &[String],
) -> Result<(), ApiError> {
    fs::create_dir_all(data_dir)?;
    let path = data_dir.join(EXTENSION_TOKENS_FILE);
    let data = serde_json::to_vec_pretty(&ExtensionTokensFile {
        token_hashes: hashes.to_vec(),
    })?;
    write_private_file(&path, &data)?;
    Ok(())
}

fn write_private_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::{io::Write as _, os::unix::fs::OpenOptionsExt as _};
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(data)?;
        file.sync_all()?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::write(path, data)
    }
}
