//! Private JSON-lines protocol between the engine and its child process.
//!
//! Each request has exactly one reply. Only one request is in flight per process.
//! Standard error is separate from the protocol. Frames can contain session secrets.

use std::fmt;

use serde::{Deserialize, Serialize};
pub use url::Url;

pub const MAX_FRAME: usize = 16 * 1024 * 1024;
pub const MAX_HTML: usize = 2 * 1024 * 1024;
pub const MAX_RESOURCES: usize = 512;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Locale {
    #[default]
    En,
    It,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum View {
    Inspect,
    Verify(Locale),
    Resolve,
    Session,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Open {
        url: Url,
        view: View,
        #[serde(default)]
        steps: Vec<Step>,
    },
    Import {
        url: Url,
        cookies: Vec<String>,
    },
    Shutdown,
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request").finish_non_exhaustive()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    Get {
        path: String,
    },
    WaitFor {
        selector: String,
    },
    Click {
        selector: String,
    },
    Submit {
        selector: String,
        button: Option<String>,
        fields: Vec<(String, String)>,
    },
    Wait {
        seconds: Option<u64>,
        selector: Option<String>,
        default_seconds: Option<u64>,
    },
    Extract {
        selector: Option<String>,
        regex: Option<String>,
        attribute: Option<String>,
    },
    Follow,
    ExpectDownload,
}

impl fmt::Debug for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Step").finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Page {
        url: Url,
        html: String,
        resources: Vec<Url>,
        session: Session,
    },
    Download {
        url: Url,
        referer: Url,
        session: Session,
    },
    Session {
        session: Session,
    },
    Imported {
        count: usize,
    },
    Error {
        kind: Failure,
    },
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Response").finish_non_exhaustive()
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Session {
    pub user_agent: String,
    /// Set-Cookie representations scoped to the origin used for the request.
    pub cookies: Vec<String>,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    #[error("the browser is unavailable")]
    Unavailable,
    #[error("the browser profile is in use")]
    ProfileInUse,
    #[error("the browser did not complete the request in time")]
    Timeout,
    #[error("the browser request was cancelled")]
    Cancelled,
    #[error("the host requires browser verification")]
    VerificationRequired,
    #[error("the page did not expose a download")]
    NoDownload,
    #[error("the browser request is invalid")]
    InvalidRequest,
}

pub fn web_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
}

pub fn challenge_url(url: &Url) -> bool {
    url.path().starts_with("/cdn-cgi/challenge-platform/")
        || url
            .query_pairs()
            .any(|(key, _)| key.starts_with("__cf_chl_"))
}

/// Used in both the native DOM and the resolver HTML parser.
pub const CHALLENGE_SELECTOR: &str = concat!(
    "form#challenge-form, form:has(input[type=password]), ",
    ".g-recaptcha[data-sitekey], .h-captcha[data-sitekey], .cf-turnstile[data-sitekey], ",
    "iframe[src*='challenges.cloudflare.com'], iframe[src*='recaptcha/'], ",
    "iframe[src*='hcaptcha.com'], input[name=captcha_code]"
);
