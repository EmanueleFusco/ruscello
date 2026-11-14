use std::{collections::BTreeMap, num::NonZeroUsize, path::Path, time::Duration};

use regex::Regex;
use reqwest::{Response, Url, header};
use scraper::{Html, Selector};
use serde::Deserialize;
use tokio::io::AsyncReadExt as _;

use crate::{Context, HostPlugin, HosterLimits, Resolution, ResolveError, ResolveFuture, http};

#[derive(Debug, Clone)]
pub struct Recipe {
    config: Config,
    patterns: Vec<Regex>,
    pub steps: Vec<RecipeStep>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    name: String,
    #[serde(rename = "match")]
    patterns: Vec<String>,
    max_connections: Option<NonZeroUsize>,
    max_parallel: Option<NonZeroUsize>,
    #[serde(default)]
    browser: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    recipe: Config,
    #[serde(rename = "step")]
    steps: Vec<RecipeStep>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RecipeStep {
    Get(String),
    SubmitForm {
        selector: String,
        button: Option<String>,
        #[serde(default)]
        fields: BTreeMap<String, String>,
    },
    Wait {
        seconds: Option<u64>,
        from_page: Option<String>,
        default_seconds: Option<u64>,
    },
    Extract {
        css: Option<String>,
        regex: Option<String>,
        attribute: Option<String>,
    },
    Follow(bool),
    ExpectDownload(bool),
    Click {
        selector: String,
    },
    WaitFor {
        selector: String,
    },
}

impl Recipe {
    pub(crate) fn xfs_browser() -> Self {
        // Every action is still gated by the page's native form and countdown.
        let submit = |operation: &str, button: &str| RecipeStep::SubmitForm {
            selector: format!("form:has(input[name=op][value={operation}])"),
            button: Some(button.to_owned()),
            fields: BTreeMap::new(),
        };
        Self {
            config: Config { name: "XFileSharing".into(), patterns: Vec::new(), max_connections: Some(NonZeroUsize::MIN), max_parallel: Some(NonZeroUsize::MIN), browser: true },
            patterns: Vec::new(),
            steps: vec![
                submit("download1", "#method_free, [name=method_free]"),
                RecipeStep::WaitFor { selector: "#downloadbtn, #downloadlink, #direct_link".into() },
                RecipeStep::Wait { seconds: None, from_page: Some("#countdown [data-seconds], #countdown span, #countdown, #seconds, [data-countdown]".into()), default_seconds: Some(0) },
                RecipeStep::Click { selector: "#downloadbtn, #downloadlink, #direct_link".into() },
                RecipeStep::ExpectDownload(true),
            ],
        }
    }

    pub fn parse(raw: &str) -> Result<Self, ResolveError> {
        let file: File = toml::from_str(raw).map_err(|_| ResolveError::InvalidRecipe)?;
        if file.recipe.name.trim().is_empty()
            || file.recipe.patterns.is_empty()
            || file.steps.is_empty()
            || file.steps.len() > 32
        {
            return Err(ResolveError::InvalidRecipe);
        }
        let patterns = file
            .recipe
            .patterns
            .iter()
            .map(|pattern| regex(pattern))
            .collect::<Result<_, _>>()?;
        for step in &file.steps {
            step.validate(file.recipe.browser)?;
        }
        Ok(Self {
            config: file.recipe,
            patterns,
            steps: file.steps,
        })
    }
}

impl RecipeStep {
    fn validate(&self, browser: bool) -> Result<(), ResolveError> {
        let selectors: Vec<&str> = match self {
            Self::SubmitForm {
                selector, button, ..
            } => std::iter::once(selector.as_str())
                .chain(button.as_deref())
                .collect(),
            Self::Wait {
                seconds,
                from_page,
                default_seconds,
            } => {
                if (seconds.is_some() == from_page.is_some())
                    || seconds
                        .or(*default_seconds)
                        .is_some_and(|seconds| seconds > 3600)
                {
                    return Err(ResolveError::InvalidRecipe);
                }
                from_page.as_deref().into_iter().collect()
            }
            Self::Extract {
                css,
                regex: expression,
                ..
            } => {
                if css.is_some() == expression.is_some() {
                    return Err(ResolveError::InvalidRecipe);
                }
                if let Some(expression) = expression {
                    regex(expression)?;
                }
                css.as_deref().into_iter().collect()
            }
            Self::Click { selector } | Self::WaitFor { selector } if browser => vec![selector],
            Self::Click { .. }
            | Self::WaitFor { .. }
            | Self::Follow(false)
            | Self::ExpectDownload(false) => {
                return Err(ResolveError::InvalidRecipe);
            }
            _ => Vec::new(),
        };
        if selectors
            .iter()
            .any(|selector| Selector::parse(selector).is_err())
        {
            return Err(ResolveError::InvalidRecipe);
        }
        Ok(())
    }
}

fn regex(expression: &str) -> Result<Regex, ResolveError> {
    Regex::new(expression).map_err(|_| ResolveError::InvalidRecipe)
}

impl HostPlugin for Recipe {
    fn name(&self) -> &str {
        &self.config.name
    }
    fn matches(&self, url: &Url) -> bool {
        self.patterns
            .iter()
            .any(|pattern| pattern.is_match(url.as_str()))
    }
    fn requires_browser(&self) -> bool {
        self.config.browser
    }
    fn limits(&self) -> HosterLimits {
        HosterLimits {
            connections: self.config.max_connections,
            parallel_downloads: self.config.max_parallel,
        }
    }
    fn resolve<'a>(&'a self, ctx: &'a Context, url: &'a Url) -> ResolveFuture<'a, Resolution> {
        Box::pin(self.run(ctx, url))
    }
}

impl Recipe {
    async fn run(&self, ctx: &Context, source: &Url) -> Result<Resolution, ResolveError> {
        if self.requires_browser() {
            return Err(ResolveError::VerificationRequired);
        }
        let mut page = Page {
            url: source.clone(),
            body: String::new(),
            direct: false,
        };
        let mut extracted = None;
        for step in &self.steps {
            match step {
                RecipeStep::Get(path) => {
                    let target = http::join(&page.url, path).ok_or(ResolveError::InvalidLink)?;
                    page = Page::read(ctx.get(target).send().await?).await?;
                }
                RecipeStep::SubmitForm {
                    selector,
                    button,
                    fields,
                } => {
                    let mut form = http::Form::read(
                        &Html::parse_document(&page.body),
                        &page.url,
                        selector,
                        button.as_deref(),
                    )?;
                    for (name, value) in fields {
                        form.set(name.clone(), value.clone());
                    }
                    page = Page::read(form.send(ctx, &page.url).await?).await?;
                }
                RecipeStep::Wait {
                    seconds,
                    from_page,
                    default_seconds,
                } => {
                    let wait = seconds
                        .or_else(|| {
                            let document = Html::parse_document(&page.body);
                            http::first(&document, from_page.as_deref()?)?
                                .text()
                                .collect::<String>()
                                .trim()
                                .parse::<u64>()
                                .ok()
                        })
                        .or(*default_seconds)
                        .filter(|seconds| *seconds <= 3600)
                        .ok_or(ResolveError::NoDownload)?;
                    ctx.wait(Duration::from_secs(wait)).await?;
                }
                RecipeStep::Extract {
                    css,
                    regex: expression,
                    attribute,
                } => {
                    let result = if let Some(selector) = css {
                        let document = Html::parse_document(&page.body);
                        let element =
                            http::first(&document, selector).ok_or(ResolveError::NoDownload)?;
                        match attribute {
                            Some(attribute) => element.attr(attribute).map(str::to_owned),
                            None => element
                                .attr("href")
                                .or_else(|| element.attr("src"))
                                .map_or_else(
                                    || Some(element.text().collect()),
                                    |value| Some(value.to_owned()),
                                ),
                        }
                    } else if let Some(expression) = expression {
                        regex(expression)?
                            .captures(&page.body)
                            .and_then(|captures| captures.get(1).or_else(|| captures.get(0)))
                            .map(|value| value.as_str().to_owned())
                    } else {
                        None
                    };
                    extracted = result.and_then(|value| http::join(&page.url, &value));
                    if extracted.is_none() {
                        return Err(ResolveError::NoDownload);
                    }
                }
                RecipeStep::Follow(true) => {
                    let target = extracted.take().ok_or(ResolveError::NoDownload)?;
                    page = Page::read(
                        ctx.get(target)
                            .header(header::REFERER, page.url.as_str())
                            .send()
                            .await?,
                    )
                    .await?;
                }
                RecipeStep::ExpectDownload(true) if page.direct => {
                    return Ok(ctx.direct(page.url, source, self.limits()));
                }
                _ => return Err(ResolveError::NoDownload),
            }
        }
        let target = extracted
            .or_else(|| page.direct.then_some(page.url))
            .ok_or(ResolveError::NoDownload)?;
        Ok(ctx.direct(target, source, self.limits()))
    }
}

struct Page {
    url: Url,
    body: String,
    direct: bool,
}

impl Page {
    async fn read(response: Response) -> Result<Self, ResolveError> {
        let url = response.url().clone();
        let direct = !http::is_html(&response);
        let body = if direct {
            http::status(&response)?;
            String::new()
        } else {
            http::page(response).await?
        };
        Ok(Self { url, body, direct })
    }
}

pub(crate) async fn load(directory: Option<&Path>) -> Result<Vec<Recipe>, ResolveError> {
    let Some(directory) = directory else {
        return Ok(Vec::new());
    };
    let mut listing = match tokio::fs::read_dir(directory).await {
        Ok(listing) => listing,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    let mut files = Vec::new();
    while let Some(entry) = listing.next_entry().await? {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "toml") && entry.file_type().await?.is_file() {
            files.push(path);
        }
    }
    files.sort_unstable();
    let mut result = Vec::new();
    for path in files {
        let mut raw = String::new();
        let file = tokio::fs::File::open(&path).await?;
        file.take(65537).read_to_string(&mut raw).await?;
        if raw.len() > 65536 {
            return Err(ResolveError::RecipeFile(path));
        }
        result.push(Recipe::parse(&raw).map_err(|_| ResolveError::RecipeFile(path))?);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Resolver;
    use ruscello_testkit::{Reply, TestServer};
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn reads_updated_recipes_and_validates_browser_steps()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("custom.toml");
        let recipe = r##"
            [recipe]
            name = "Local fixture"
            match = ['^http://127\.0\.0\.1:[0-9]+/share$']
            max_connections = 1
            [[step]]
            get = ""
            [[step]]
            extract = { css = "#SELECTOR" }
        "##;
        let recipe = recipe.replace("#SELECTOR", "a#initial");
        tokio::fs::write(&path, &recipe).await?;
        let server = TestServer::start().await?;
        let url = Url::parse(&server.route("/share", |_| {
            Reply::html(
                r#"<a id="initial" href="/dl/one">One</a><a id="later" href="/dl/two">Two</a>"#,
            )
        }))?;
        let resolver = Resolver::new(Some(dir.path().to_path_buf()))?;
        let cancellation = CancellationToken::new();
        let first = resolver
            .resolve(&url, &cancellation)
            .await?
            .ok_or("expected link")?;
        assert_eq!(first.url.path(), "/dl/one");
        tokio::fs::write(path, recipe.replace("a#initial", "a#later")).await?;
        let second = resolver
            .resolve(&url, &cancellation)
            .await?
            .ok_or("expected link")?;
        assert_eq!(second.url.path(), "/dl/two");
        let browser = r#"
            [recipe]
            name = "DOM fixture"
            match = [".*"]
            browser = true
            [[step]]
            click = { selector = "button.download" }
            [[step]]
            wait_for = { selector = "a.file" }
        "#;
        assert!(Recipe::parse(browser)?.requires_browser());
        assert!(Recipe::parse(&browser.replace("browser = true", "browser = false")).is_err());
        Ok(())
    }
}
