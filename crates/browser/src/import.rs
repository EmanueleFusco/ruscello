//! Explicit, site-scoped import from installed browsers' readable SQLite profiles.

use std::{
    collections::BTreeMap,
    env, io,
    path::{Path, PathBuf},
};

use cookie::{Cookie, SameSite, time::OffsetDateTime};
use rusqlite::{Connection, OpenFlags};

use crate::protocol::{Url, web_url};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Browser {
    Firefox,
    Chromium,
}

/// Reads cookies for the selected host. Profiles and cookie values stay out of logs.
/// Chromium profiles protected by OS encryption usually have no readable values.
pub fn installed(browser: Browser, site: &Url) -> Vec<String> {
    if !web_url(site) {
        return Vec::new();
    }
    let mut result = BTreeMap::new();
    for root in roots(browser) {
        let mut profiles = std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        profiles.sort();
        profiles.insert(0, root);
        for profile in profiles {
            let databases = match browser {
                Browser::Firefox => vec![profile.join("cookies.sqlite")],
                Browser::Chromium => vec![profile.join("Network/Cookies"), profile.join("Cookies")],
            };
            for path in databases {
                for cookie in read(&path, browser, site).unwrap_or_default() {
                    let key = (
                        cookie.name().to_owned(),
                        cookie.domain().map(str::to_owned),
                        cookie.path().map(str::to_owned),
                    );
                    result.entry(key).or_insert(cookie);
                }
            }
        }
    }
    result
        .into_values()
        .map(|cookie| cookie.to_string())
        .collect()
}

fn roots(browser: Browser) -> Vec<PathBuf> {
    let join = |root: Option<std::ffi::OsString>, names: &[&str]| {
        root.into_iter()
            .flat_map(|root| {
                let root = PathBuf::from(root);
                names.iter().map(move |name| root.join(name))
            })
            .collect::<Vec<_>>()
    };
    if cfg!(target_os = "windows") {
        return match browser {
            Browser::Firefox => join(env::var_os("APPDATA"), &["Mozilla/Firefox/Profiles"]),
            Browser::Chromium => join(
                env::var_os("LOCALAPPDATA"),
                &[
                    "Google/Chrome/User Data",
                    "Chromium/User Data",
                    "Microsoft/Edge/User Data",
                    "BraveSoftware/Brave-Browser/User Data",
                ],
            ),
        };
    }
    if cfg!(target_os = "macos") {
        return match browser {
            Browser::Firefox => join(
                env::var_os("HOME"),
                &["Library/Application Support/Firefox/Profiles"],
            ),
            Browser::Chromium => join(
                env::var_os("HOME"),
                &[
                    "Library/Application Support/Google/Chrome",
                    "Library/Application Support/Chromium",
                    "Library/Application Support/Microsoft Edge",
                    "Library/Application Support/BraveSoftware/Brave-Browser",
                ],
            ),
        };
    }
    match browser {
        Browser::Firefox => join(
            env::var_os("HOME"),
            &[
                ".mozilla/firefox",
                "snap/firefox/common/.mozilla/firefox",
                ".var/app/org.mozilla.firefox/.mozilla/firefox",
            ],
        ),
        Browser::Chromium => join(
            env::var_os("XDG_CONFIG_HOME").or_else(|| {
                env::var_os("HOME").map(|home| PathBuf::from(home).join(".config").into_os_string())
            }),
            &[
                "google-chrome",
                "chromium",
                "microsoft-edge",
                "BraveSoftware/Brave-Browser",
            ],
        ),
    }
}

fn read(path: &Path, browser: Browser, url: &Url) -> Result<Vec<Cookie<'static>>, rusqlite::Error> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(std::time::Duration::from_millis(150))?;
    let sql = match browser {
        Browser::Firefox => {
            "SELECT name, value, host, path, expiry, isSecure, isHttpOnly, sameSite FROM moz_cookies WHERE (host = ?1 OR substr(host, 1, 1) = '.') AND originAttributes = ''"
        }
        Browser::Chromium => {
            "SELECT name, value, host_key, path, expires_utc / 1000000 - 11644473600, is_secure, is_httponly, samesite FROM cookies WHERE (host_key = ?1 OR substr(host_key, 1, 1) = '.') AND length(encrypted_value) = 0 AND top_frame_site_key = ''"
        }
    };
    let mut statement = db.prepare(sql)?;
    let host = url.host_str().unwrap_or_default();
    let mapped = statement.query_map([host], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, bool>(5)?,
            row.get::<_, bool>(6)?,
            row.get::<_, i64>(7)?,
        ))
    })?;
    let now = OffsetDateTime::now_utc();
    Ok(mapped
        .filter_map(Result::ok)
        .filter_map(
            |(name, value, domain, path, expiry, secure, http_only, same_site)| {
                let expiry = OffsetDateTime::from_unix_timestamp(expiry).ok()?;
                if expiry <= now
                    || (secure && url.scheme() != "https")
                    || !matches_site(host, &domain)
                {
                    return None;
                }
                let mut cookie = Cookie::build((name, value))
                    .path(path)
                    .expires(expiry)
                    .secure(secure)
                    .http_only(http_only);
                if domain.starts_with('.') {
                    cookie = cookie.domain(domain);
                }
                cookie = match same_site {
                    0 if browser == Browser::Chromium => cookie.same_site(SameSite::None),
                    1 => cookie.same_site(SameSite::Lax),
                    2 => cookie.same_site(SameSite::Strict),
                    _ => cookie,
                };
                Some(cookie.build())
            },
        )
        .take(512)
        .collect())
}

fn matches_site(host: &str, domain: &str) -> bool {
    match domain.strip_prefix('.') {
        Some(domain) if domain.contains('.') => {
            host == domain
                || host
                    .strip_suffix(domain)
                    .is_some_and(|prefix| prefix.ends_with('.'))
        }
        None => host == domain,
        _ => false,
    }
}

/// Validates a supplied cookie before giving it to the native backend.
pub fn for_site(value: &str, url: &Url) -> io::Result<Cookie<'static>> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid imported cookie");
    let mut cookie = Cookie::parse(value.to_owned()).map_err(|_| invalid())?;
    let host = url
        .host_str()
        .filter(|_| web_url(url))
        .ok_or_else(invalid)?;
    if cookie
        .domain()
        .is_some_and(|domain| host != domain && !matches_site(host, &format!(".{domain}")))
        || cookie.secure() == Some(true) && url.scheme() != "https"
        || cookie.name().is_empty()
        || !cookie
            .name()
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
        || !cookie
            .value()
            .bytes()
            .all(|byte| (0x21..0x7f).contains(&byte) && !b";,\\".contains(&byte))
    {
        return Err(invalid());
    }
    if cookie.domain().is_none() {
        // Native cookie APIs take a domain. Without a leading dot it is host-only.
        cookie.set_domain(host.to_owned());
    }
    Ok(cookie)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firefox_import_preserves_site_scope_and_http_only() -> Result<(), Box<dyn std::error::Error>>
    {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("cookies.sqlite");
        let db = Connection::open(&path)?;
        db.execute_batch("CREATE TABLE moz_cookies (name TEXT, value TEXT, host TEXT, path TEXT, expiry INTEGER, isSecure INTEGER, isHttpOnly INTEGER, sameSite INTEGER, originAttributes TEXT);
            INSERT INTO moz_cookies VALUES ('valid','data','.example.test','/download',4000000000,1,1,1,''),
                ('expired','data','.example.test','/',1,1,0,1,''),
                ('private','data','.example.test','/',4000000000,1,0,1,'^userContextId=2'),
                ('different','data','.other.test','/',4000000000,1,0,1,''),
                ('hostOnly','data','example.test','/',4000000000,1,0,1,'');")?;
        let url = Url::parse("https://downloads.example.test")?;
        let cookies = read(&path, Browser::Firefox, &url)?;
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].name(), "valid");
        assert_eq!(cookies[0].path(), Some("/download"));
        assert_eq!(cookies[0].http_only(), Some(true));
        assert!(
            read(
                &path,
                Browser::Firefox,
                &Url::parse("https://notexample.test")?
            )?
            .is_empty()
        );
        assert!(
            read(
                &path,
                Browser::Firefox,
                &Url::parse("http://downloads.example.test")?
            )?
            .is_empty()
        );
        Ok(())
    }
}
