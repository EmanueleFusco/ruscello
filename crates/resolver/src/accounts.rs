use crate::{AccountStore, ResolveError, ResolveFuture};
use serde::{Deserialize, Serialize};

const SERVICE: &str = "Ruscello Hoster Accounts";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HosterAccount {
    pub host: String,
    pub username: String,
}

#[derive(Serialize, Deserialize)]
pub struct HosterCredentials {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for HosterCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HosterCredentials")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct KeyringAccounts;

impl AccountStore for KeyringAccounts {
    fn get(&self, host: String) -> ResolveFuture<'_, Option<HosterCredentials>> {
        Box::pin(Self::get(host))
    }
}

impl KeyringAccounts {
    pub async fn set(
        host: String,
        credentials: HosterCredentials,
    ) -> Result<HosterAccount, ResolveError> {
        if credentials.password.is_empty() {
            return Err(ResolveError::InvalidAccount);
        }
        let host = normalize(&host)?;
        blocking(move || {
            let secret =
                serde_json::to_string(&credentials).map_err(|_| ResolveError::InvalidAccount)?;
            entry(&host)?
                .set_password(&secret)
                .map_err(|_| ResolveError::Keyring)?;
            Ok(HosterAccount {
                host,
                username: credentials.username,
            })
        })
        .await
    }

    pub async fn get(host: String) -> Result<Option<HosterCredentials>, ResolveError> {
        let host = normalize(&host)?;
        blocking(move || match entry(&host)?.get_password() {
            Ok(value) => serde_json::from_str(&value)
                .map(Some)
                .map_err(|_| ResolveError::Keyring),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(ResolveError::Keyring),
        })
        .await
    }

    pub async fn remove(host: String) -> Result<(), ResolveError> {
        let host = normalize(&host)?;
        blocking(move || match entry(&host)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(ResolveError::Keyring),
        })
        .await
    }
}

fn normalize(host: &str) -> Result<String, ResolveError> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    if host.is_empty()
        || host.len() > 253
        || !host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return Err(ResolveError::InvalidAccount);
    }
    Ok(host.to_owned())
}

fn entry(host: &str) -> Result<keyring::Entry, ResolveError> {
    keyring::Entry::new(SERVICE, host).map_err(|_| ResolveError::Keyring)
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ResolveError> + Send + 'static,
) -> Result<T, ResolveError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ResolveError::Keyring)?
}
