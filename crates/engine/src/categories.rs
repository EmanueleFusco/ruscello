use std::{
    collections::BTreeMap,
    num::{NonZeroU64, NonZeroUsize},
};

use regex::{Regex, RegexBuilder};
use ruscello_core::{Category, CategoryId, DefaultCategory, SourceKind};
use ruscello_http::{BandwidthLimit, Url};
use ruscello_storage::{Change, Snapshot, Storage};
use serde::{Deserialize, Serialize};

use crate::EngineError;

const KEY: &str = "category_settings";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryLimits {
    pub concurrent_downloads: Option<NonZeroUsize>,
    pub bandwidth: Option<NonZeroU64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryPolicy {
    pub category: Category,
    pub limits: CategoryLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CategoryMatch {
    /// Matches the host or one of its subdomains.
    Domain(String),
    /// Matches the filename hint or final component of the source URL.
    Extension(String),
    /// Matches the source URL or the filename when available.
    Regex(String),
    Source(SourceKind),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryRule {
    pub matcher: CategoryMatch,
    pub category: CategoryId,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CategorySettings {
    pub categories: Vec<CategoryPolicy>,
    /// First matching rule wins. An explicit category takes precedence.
    pub rules: Vec<CategoryRule>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SavedPolicy {
    rules: Vec<CategoryRule>,
    limits: BTreeMap<CategoryId, CategoryLimits>,
}

pub(super) struct CategoryBook {
    categories: BTreeMap<CategoryId, CategoryPolicy>,
    rules: Vec<CategoryRule>,
    compiled: Vec<Matcher>,
    buckets: BTreeMap<CategoryId, BandwidthLimit>,
}

enum Matcher {
    Domain(String),
    Extension(String),
    Regex(Regex),
    Source(SourceKind),
}

impl CategoryBook {
    pub(super) async fn load(storage: &Storage, snapshot: &Snapshot) -> Result<Self, EngineError> {
        let saved: Option<SavedPolicy> = snapshot
            .settings
            .get(KEY)
            .cloned()
            .map(serde_json::from_value)
            .transpose()?;
        let initialize = saved.is_none();
        let saved = saved.unwrap_or_default();
        let mut categories = snapshot.categories.clone();
        if initialize {
            for default in DefaultCategory::ALL {
                if !categories
                    .iter()
                    .any(|category| category.id == default.id())
                {
                    categories.push(Category {
                        id: default.id(),
                        name: default.english_name().into(),
                        destination: None,
                    });
                }
            }
        }
        let settings = CategorySettings {
            categories: categories
                .into_iter()
                .map(|category| CategoryPolicy {
                    limits: saved.limits.get(&category.id).copied().unwrap_or_default(),
                    category,
                })
                .collect(),
            rules: saved.rules,
        };
        let book = tokio::task::spawn_blocking(move || Self::new(settings)).await??;
        if initialize {
            storage.apply(book.changes(None)?).await?;
        }
        Ok(book)
    }

    pub(super) fn new(settings: CategorySettings) -> Result<Self, EngineError> {
        let mut categories = BTreeMap::new();
        let mut buckets = BTreeMap::new();
        for mut policy in settings.categories {
            let id = policy.category.id;
            if id.0 == 0
                || id.0 > i64::MAX as u64
                || policy.category.name.trim().is_empty()
                || categories.contains_key(&id)
            {
                return Err(EngineError::InvalidCategory);
            }
            policy.category.destination = policy
                .category
                .destination
                .as_deref()
                .map(std::path::absolute)
                .transpose()?;
            buckets.insert(id, BandwidthLimit::new(policy.limits.bandwidth));
            categories.insert(id, policy);
        }
        let compiled = settings
            .rules
            .iter()
            .enumerate()
            .map(|(index, rule)| {
                categories
                    .contains_key(&rule.category)
                    .then(|| Matcher::compile(&rule.matcher))
                    .flatten()
                    .ok_or(EngineError::InvalidCategoryRule(index + 1))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            categories,
            rules: settings.rules,
            compiled,
            buckets,
        })
    }

    pub(super) fn changes(&self, previous: Option<&Self>) -> Result<Vec<Change>, EngineError> {
        let saved = SavedPolicy {
            rules: self.rules.clone(),
            limits: self
                .categories
                .iter()
                .map(|(&id, policy)| (id, policy.limits))
                .collect(),
        };
        let mut changes = vec![Change::SaveSetting {
            key: KEY.into(),
            value: serde_json::to_value(saved)?,
        }];
        for policy in self.categories.values() {
            let old = previous.and_then(|book| book.get(policy.category.id));
            if old.is_none_or(|old| old.category != policy.category) {
                changes.push(Change::SaveCategory(policy.category.clone()));
            }
        }
        if let Some(previous) = previous {
            changes.extend(
                previous
                    .categories
                    .keys()
                    .filter(|id| !self.categories.contains_key(id))
                    .copied()
                    .map(Change::RemoveCategory),
            );
        }
        Ok(changes)
    }

    pub(super) fn reuse_buckets(&mut self, previous: &mut Self) {
        for (id, bucket) in std::mem::take(&mut previous.buckets) {
            let rate = self.get(id).and_then(|policy| policy.limits.bandwidth);
            bucket.set_rate(rate);
            if self.categories.contains_key(&id) {
                self.buckets.insert(id, bucket);
            }
        }
    }

    pub(super) fn settings(&self) -> CategorySettings {
        CategorySettings {
            categories: self.categories.values().cloned().collect(),
            rules: self.rules.clone(),
        }
    }

    pub(super) fn get(&self, id: CategoryId) -> Option<&CategoryPolicy> {
        self.categories.get(&id)
    }

    pub(super) fn bucket(&self, id: Option<CategoryId>) -> BandwidthLimit {
        id.and_then(|id| self.buckets.get(&id))
            .cloned()
            .unwrap_or_default()
    }

    pub(super) fn assign(
        &self,
        explicit: Option<CategoryId>,
        kind: SourceKind,
        url: &Url,
        filename: Option<&str>,
    ) -> Result<Option<&CategoryPolicy>, EngineError> {
        if let Some(id) = explicit {
            return self.get(id).map(Some).ok_or(EngineError::UnknownCategory);
        }
        let decoded = url
            .path_segments()
            .and_then(|mut parts| parts.next_back())
            .map(|part| percent_encoding::percent_decode_str(part).decode_utf8_lossy());
        let filename = filename.or(decoded.as_deref()).unwrap_or_default();
        let id = self
            .compiled
            .iter()
            .zip(&self.rules)
            .find_map(|(matcher, rule)| {
                matcher
                    .matches(kind, url, filename)
                    .then_some(rule.category)
            });
        Ok(id.and_then(|id| self.get(id)))
    }
}

impl Matcher {
    fn compile(matcher: &CategoryMatch) -> Option<Self> {
        match matcher {
            CategoryMatch::Domain(domain) => {
                let domain = domain.trim().trim_matches('.');
                let url = Url::parse(&format!("https://{domain}/")).ok()?;
                if url.path() != "/"
                    || url.query().is_some()
                    || url.fragment().is_some()
                    || url.port().is_some()
                    || !url.username().is_empty()
                    || url.password().is_some()
                {
                    return None;
                }
                Some(Self::Domain(url.host_str()?.to_owned()))
            }
            CategoryMatch::Extension(extension) => {
                let extension = extension
                    .trim()
                    .trim_start_matches('.')
                    .to_ascii_lowercase();
                (!extension.is_empty()
                    && extension
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.'))
                .then_some(Self::Extension(extension))
            }
            CategoryMatch::Regex(pattern) => RegexBuilder::new(pattern)
                .size_limit(1024 * 1024)
                .build()
                .ok()
                .map(Self::Regex),
            CategoryMatch::Source(kind) => Some(Self::Source(*kind)),
        }
    }

    fn matches(&self, kind: SourceKind, url: &Url, filename: &str) -> bool {
        match self {
            Self::Domain(domain) => url.host_str().is_some_and(|host| {
                let host = host.trim_end_matches('.');
                host == domain
                    || host
                        .strip_suffix(domain)
                        .is_some_and(|prefix| prefix.ends_with('.'))
            }),
            Self::Extension(extension) => filename
                .to_ascii_lowercase()
                .strip_suffix(extension)
                .is_some_and(|prefix| prefix.len() > 1 && prefix.ends_with('.')),
            Self::Regex(regex) => regex.is_match(url.as_str()) || regex.is_match(filename),
            Self::Source(source) => *source == kind,
        }
    }
}
