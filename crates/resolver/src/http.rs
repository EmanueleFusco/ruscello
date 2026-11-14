use std::time::Duration;

use reqwest::{
    ClientBuilder, Response, Url,
    header::{self, HeaderMap, HeaderValue},
};
use ruscello_browser::protocol::{CHALLENGE_SELECTOR, challenge_url};
use scraper::{ElementRef, Html, Selector};

use crate::{Context, HosterLimits, Resolution, ResolveError};

pub(crate) const PAGE_LIMIT: usize = 2 * 1024 * 1024;

pub(crate) fn client() -> ClientBuilder {
    reqwest::Client::builder()
        .user_agent(concat!("Ruscello/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(25))
        .referer(false)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                attempt.error("too many redirects")
            } else if validate(attempt.url()).is_err() {
                attempt.error("invalid redirect URL")
            } else if matches!(
                attempt.status(),
                reqwest::StatusCode::TEMPORARY_REDIRECT | reqwest::StatusCode::PERMANENT_REDIRECT
            ) && attempt
                .previous()
                .last()
                .is_some_and(|previous| previous.origin() != attempt.url().origin())
            {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
}

pub(crate) fn validate(url: &Url) -> Result<(), ResolveError> {
    if matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
    {
        Ok(())
    } else {
        Err(ResolveError::InvalidLink)
    }
}

pub(crate) fn join(base: &Url, value: &str) -> Option<Url> {
    let mut url = base.join(value.trim()).ok()?;
    validate(&url).ok()?;
    url.set_fragment(None);
    Some(url)
}

pub(crate) fn direct(
    url: Url,
    referer: &Url,
    limits: HosterLimits,
    mut headers: HeaderMap,
) -> Resolution {
    let mut referer = referer.clone();
    referer.set_fragment(None);
    if referer.origin() != url.origin() {
        referer.set_query(None);
        referer.set_path("/");
    }
    if (referer.scheme() != "https" || url.scheme() == "https")
        && let Ok(mut value) = HeaderValue::from_str(referer.as_str())
    {
        value.set_sensitive(true);
        headers.insert(header::REFERER, value);
    }
    Resolution {
        url,
        headers,
        limits,
        expires: None,
        filename: None,
    }
}

pub(crate) fn attachment(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("attachment"))
}

pub(crate) fn is_html(response: &Response) -> bool {
    !attachment(response)
        && response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                let mime = value.split(';').next().unwrap_or_default().trim();
                mime.eq_ignore_ascii_case("text/html")
                    || mime.eq_ignore_ascii_case("application/xhtml+xml")
            })
}

pub(crate) fn status(response: &Response) -> Result<(), ResolveError> {
    if response
        .headers()
        .get("cf-mitigated")
        .is_some_and(|value| value == "challenge")
        || response.headers().get("cf-chl-out").is_some()
        || challenge_url(response.url())
    {
        Err(ResolveError::VerificationRequired)
    } else if !response.status().is_success() {
        Err(ResolveError::Status(response.status()))
    } else {
        Ok(())
    }
}

pub(crate) async fn body(response: Response, limit: usize) -> Result<String, ResolveError> {
    status(&response)?;
    read_body(response, limit).await
}

async fn read_body(mut response: Response, limit: usize) -> Result<String, ResolveError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(ResolveError::TooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(ResolveError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) async fn page(response: Response) -> Result<String, ResolveError> {
    status(&response).or_else(|error| {
        if is_html(&response)
            && matches!(
                error,
                ResolveError::Status(
                    reqwest::StatusCode::UNAUTHORIZED
                        | reqwest::StatusCode::FORBIDDEN
                        | reqwest::StatusCode::SERVICE_UNAVAILABLE
                )
            )
        {
            Ok(())
        } else {
            Err(error)
        }
    })?;
    let status_code = response.status();
    let contents = read_body(response, PAGE_LIMIT).await?;
    let document = Html::parse_document(&contents);
    let challenge = contents.contains("/cdn-cgi/challenge-platform/")
        || first(&document, CHALLENGE_SELECTOR).is_some();
    if challenge {
        return Err(ResolveError::VerificationRequired);
    }
    if !status_code.is_success() {
        return Err(ResolveError::Status(status_code));
    }
    Ok(contents)
}

pub(crate) fn first<'a>(document: &'a Html, query: &str) -> Option<ElementRef<'a>> {
    document.select(&Selector::parse(query).ok()?).next()
}

#[derive(Debug)]
pub(crate) struct Form {
    url: Url,
    post: bool,
    fields: Vec<(String, String)>,
}

impl Form {
    pub(crate) fn read(
        document: &Html,
        base: &Url,
        selector: &str,
        button: Option<&str>,
    ) -> Result<Self, ResolveError> {
        let element = first(document, selector).ok_or(ResolveError::NoDownload)?;
        let url = join(base, element.attr("action").unwrap_or_default())
            .ok_or(ResolveError::InvalidLink)?;
        if url.origin() != base.origin() {
            return Err(ResolveError::InvalidLink);
        }
        let post = element
            .attr("method")
            .is_some_and(|value| value.eq_ignore_ascii_case("post"));
        let inputs = Selector::parse("input[name]:not([disabled])")
            .map_err(|_| ResolveError::InvalidRecipe)?;
        let mut fields = Vec::new();
        for input in element.select(&inputs) {
            let kind = input.attr("type").unwrap_or_default();
            if matches!(
                kind.to_ascii_lowercase().as_str(),
                "submit" | "button" | "file" | "image" | "reset"
            ) || (matches!(kind.to_ascii_lowercase().as_str(), "checkbox" | "radio")
                && input.attr("checked").is_none())
            {
                continue;
            }
            if let Some(name) = input.attr("name") {
                fields.push((
                    name.to_owned(),
                    input.attr("value").unwrap_or_default().to_owned(),
                ));
            }
        }
        if let Some(query) = button {
            let query = Selector::parse(query).map_err(|_| ResolveError::InvalidRecipe)?;
            let button = element
                .select(&query)
                .next()
                .ok_or(ResolveError::NoDownload)?;
            if let Some(name) = button.attr("name") {
                fields.push((
                    name.to_owned(),
                    button.attr("value").unwrap_or_default().to_owned(),
                ));
            }
        }
        Ok(Self { url, post, fields })
    }

    pub(crate) fn set(&mut self, name: String, value: String) {
        self.fields.retain(|(key, _)| *key != name);
        self.fields.push((name, value));
    }

    pub(crate) async fn send(self, ctx: &Context, referer: &Url) -> Result<Response, ResolveError> {
        let request = if self.post {
            ctx.post(self.url).form(&self.fields)
        } else {
            ctx.get(self.url).query(&self.fields)
        };
        Ok(request
            .header(header::REFERER, referer.as_str())
            .send()
            .await?)
    }
}
