//! Asynchronous access to the Algolia and official Firebase Hacker News APIs.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    str::FromStr,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures::{
    future::BoxFuture,
    stream::{self, StreamExt as _},
};
use reqwest::Client;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::{
    sync::Semaphore,
    time::{Instant, timeout_at},
};
use url::Url;

use crate::model::{Comment, Feed, Item, PollOption, Source, StoryPage, Thread};

const ALGOLIA_BASE_URL: &str = "https://hn.algolia.com/api/v1/";
const FIREBASE_BASE_URL: &str = "https://hacker-news.firebaseio.com/v0/";
const DEFAULT_CONCURRENCY: usize = 12;
const MAX_CONCURRENCY: usize = 12;
const MAX_RESULTS: usize = 500;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_CONFIGURED_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const DEFAULT_MAX_THREAD_NODES: usize = 5_000;
const MAX_CONFIGURED_THREAD_NODES: usize = 20_000;
const DEFAULT_MAX_THREAD_DEPTH: u32 = 64;
const MAX_CONFIGURED_THREAD_DEPTH: u32 = 128;
const DEFAULT_THREAD_TIMEOUT: Duration = Duration::from_secs(10);
#[allow(clippy::duration_suboptimal_units)] // `Duration::from_mins` is newer than the MSRV.
const MAX_CONFIGURED_THREAD_TIMEOUT: Duration = Duration::from_secs(60);

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid API base URL: {0}")]
    Url(#[from] url::ParseError),
    #[error("invalid API base URL: {0}")]
    InvalidBaseUrl(String),
    #[error("invalid JSON response: {0}")]
    Json(#[from] serde_json::Error),
    #[error("response body exceeded the {limit}-byte safety limit")]
    BodyTooLarge { limit: usize },
    #[error("{operation} exceeded its overall time limit")]
    Timeout { operation: &'static str },
    #[error("Hacker News item {0} was not found")]
    NotFound(u64),
    #[error("invalid upstream response: {0}")]
    InvalidResponse(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("Algolia request failed ({primary}); Firebase fallback also failed ({fallback})")]
    SourcesUnavailable {
        primary: Box<Self>,
        fallback: Box<Self>,
    },
}

/// The type of Algolia result requested by [`DataSource::search`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchType {
    Story,
    Comment,
    All,
}

impl SearchType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Story => "story",
            Self::Comment => "comment",
            Self::All => "all",
        }
    }

    const fn algolia_tag(self) -> Option<&'static str> {
        match self {
            Self::Story => Some("story"),
            Self::Comment => Some("comment"),
            Self::All => None,
        }
    }
}

impl fmt::Display for SearchType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SearchType {
    type Err = ParseSearchTypeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "story" | "stories" => Ok(Self::Story),
            "comment" | "comments" => Ok(Self::Comment),
            "all" | "any" => Ok(Self::All),
            _ => Err(ParseSearchTypeError(value.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown search type `{0}`")]
pub struct ParseSearchTypeError(String);

/// Object-safe asynchronous data source used by the application and tests.
pub trait DataSource: Send + Sync {
    fn feed(&self, feed: Feed, limit: usize) -> BoxFuture<'_, ApiResult<StoryPage>>;

    fn item(&self, id: u64) -> BoxFuture<'_, ApiResult<Item>>;

    fn thread(&self, id: u64) -> BoxFuture<'_, ApiResult<Thread>>;

    fn search<'a>(
        &'a self,
        query: &'a str,
        search_type: SearchType,
        limit: usize,
    ) -> BoxFuture<'a, ApiResult<StoryPage>>;
}

/// A resilient client that uses Algolia for rich data and Firebase for canonical
/// feeds and fallback reads.
#[derive(Debug, Clone)]
pub struct HybridClient {
    http: Client,
    algolia_base: Url,
    firebase_base: Url,
    gate: Arc<Semaphore>,
    max_concurrency: usize,
    max_response_bytes: usize,
    max_thread_nodes: usize,
    max_thread_depth: u32,
    thread_timeout: Duration,
}

impl Default for HybridClient {
    fn default() -> Self {
        let http = Client::builder()
            .user_agent(concat!("hnx/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            http,
            algolia_base: Url::parse(ALGOLIA_BASE_URL).expect("constant Algolia URL is valid"),
            firebase_base: Url::parse(FIREBASE_BASE_URL).expect("constant Firebase URL is valid"),
            gate: Arc::new(Semaphore::new(DEFAULT_CONCURRENCY)),
            max_concurrency: DEFAULT_CONCURRENCY,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_thread_nodes: DEFAULT_MAX_THREAD_NODES,
            max_thread_depth: DEFAULT_MAX_THREAD_DEPTH,
            thread_timeout: DEFAULT_THREAD_TIMEOUT,
        }
    }
}

impl HybridClient {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a client with alternate API roots, primarily for local proxies
    /// and deterministic wire-level tests. Missing trailing slashes are added.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Url`] when either value is not an absolute URL.
    pub fn with_base_urls(
        algolia_base: impl AsRef<str>,
        firebase_base: impl AsRef<str>,
    ) -> ApiResult<Self> {
        let mut client = Self::new();
        client.algolia_base = parse_base_url(algolia_base.as_ref())?;
        client.firebase_base = parse_base_url(firebase_base.as_ref())?;
        Ok(client)
    }

    /// Replace the HTTP transport while retaining the configured API roots.
    #[must_use]
    pub fn with_http_client(mut self, http: Client) -> Self {
        self.http = http;
        self
    }

    /// Configure request parallelism. Values are clamped to `1..=12`.
    #[must_use]
    pub fn with_max_concurrency(mut self, max_concurrency: usize) -> Self {
        let max_concurrency = max_concurrency.clamp(1, MAX_CONCURRENCY);
        self.gate = Arc::new(Semaphore::new(max_concurrency));
        self.max_concurrency = max_concurrency;
        self
    }

    #[must_use]
    pub const fn max_concurrency(&self) -> usize {
        self.max_concurrency
    }

    /// Configure the maximum decompressed JSON body retained in memory.
    /// Values are clamped to `1..=32 MiB`.
    #[must_use]
    pub fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes.clamp(1, MAX_CONFIGURED_RESPONSE_BYTES);
        self
    }

    /// Configure Firebase thread traversal caps.
    ///
    /// A depth of zero retains top-level comments but does not fetch their children. Values are
    /// capped at 20,000 comments, depth 128, and 60 seconds.
    #[must_use]
    pub fn with_thread_limits(
        mut self,
        max_nodes: usize,
        max_depth: u32,
        overall_timeout: Duration,
    ) -> Self {
        self.max_thread_nodes = max_nodes.clamp(1, MAX_CONFIGURED_THREAD_NODES);
        self.max_thread_depth = max_depth.min(MAX_CONFIGURED_THREAD_DEPTH);
        self.thread_timeout =
            overall_timeout.clamp(Duration::from_millis(1), MAX_CONFIGURED_THREAD_TIMEOUT);
        self
    }

    /// Load a canonical feed, preserving the upstream ranking order.
    ///
    /// # Errors
    ///
    /// Returns an error when the canonical provider cannot be reached or parsed. The top feed
    /// reports [`ApiError::SourcesUnavailable`] only after both providers fail.
    pub async fn feed(&self, feed: Feed, limit: usize) -> ApiResult<StoryPage> {
        let limit = bounded_limit(limit);
        if limit == 0 {
            return Ok(empty_page(
                feed,
                if feed == Feed::Top {
                    Source::Hybrid
                } else {
                    Source::Firebase
                },
            ));
        }

        if feed == Feed::Top {
            self.hybrid_top(limit).await
        } else {
            self.firebase_feed(feed, limit).await
        }
    }

    /// Load one Hacker News item, trying Firebase when Algolia is unavailable.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::InvalidRequest`] for id zero, or an upstream error after both
    /// providers fail.
    pub async fn item(&self, id: u64) -> ApiResult<Item> {
        self.item_with_source(id).await.map(|(item, _)| item)
    }

    /// Load one item together with the provider that supplied it.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::InvalidRequest`] for id zero. A Firebase `null` response is
    /// normalized to [`ApiError::NotFound`], even when Algolia failed first.
    pub async fn item_with_source(&self, id: u64) -> ApiResult<(Item, Source)> {
        if id == 0 {
            return Err(ApiError::InvalidRequest(
                "item id must be greater than zero".to_owned(),
            ));
        }

        match self.algolia_item(id).await {
            Ok(item) => Ok((item, Source::Algolia)),
            Err(primary) => match self.firebase_item(id).await {
                Ok(item) => Ok((item, Source::Firebase)),
                Err(ApiError::NotFound(_)) => Err(ApiError::NotFound(id)),
                Err(fallback) => Err(ApiError::SourcesUnavailable {
                    primary: Box::new(primary),
                    fallback: Box::new(fallback),
                }),
            },
        }
    }

    /// Load a story and its recursive comment tree.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::InvalidRequest`] for id zero, or an upstream error after both
    /// providers fail to load the root item.
    pub async fn thread(&self, id: u64) -> ApiResult<Thread> {
        if id == 0 {
            return Err(ApiError::InvalidRequest(
                "thread id must be greater than zero".to_owned(),
            ));
        }

        match self.algolia_thread(id).await {
            Ok(thread) if !thread.is_partial() => Ok(thread),
            Ok(algolia_thread) => match self.firebase_thread(id).await {
                Ok(firebase_thread) => Ok(more_complete_thread(algolia_thread, firebase_thread)),
                Err(error) => {
                    tracing::debug!(thread_id = id, %error, "retaining partial Algolia thread after Firebase fallback failed");
                    Ok(algolia_thread)
                }
            },
            Err(primary) => match self.firebase_thread(id).await {
                Ok(thread) => Ok(thread),
                Err(ApiError::NotFound(_)) => Err(ApiError::NotFound(id)),
                Err(fallback) => Err(ApiError::SourcesUnavailable {
                    primary: Box::new(primary),
                    fallback: Box::new(fallback),
                }),
            },
        }
    }

    /// Search Algolia for ranked stories, comments, or both.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::InvalidRequest`] for a blank query, or an upstream HTTP/JSON error.
    pub async fn search(
        &self,
        query: &str,
        search_type: SearchType,
        limit: usize,
    ) -> ApiResult<StoryPage> {
        let query = query.trim();
        if query.is_empty() {
            return Err(ApiError::InvalidRequest(
                "search query must not be empty".to_owned(),
            ));
        }

        let limit = bounded_limit(limit);
        if limit == 0 {
            let mut page = empty_page(Feed::Top, Source::Algolia);
            page.query = Some(query.to_owned());
            return Ok(page);
        }

        let mut url = self.algolia_url("search")?;
        {
            let mut parameters = url.query_pairs_mut();
            parameters.append_pair("query", query);
            parameters.append_pair("hitsPerPage", &limit.to_string());
            if let Some(tag) = search_type.algolia_tag() {
                parameters.append_pair("tags", tag);
            }
        }

        let response: AlgoliaSearchResponse = self.get_json(url).await?;
        let items = response
            .hits
            .into_iter()
            .filter_map(AlgoliaHit::into_item)
            .take(limit)
            .collect();

        Ok(StoryPage {
            feed: Feed::Top,
            query: Some(query.to_owned()),
            items,
            source: Source::Algolia,
            stale: false,
            fetched_at: unix_timestamp(),
        })
    }

    async fn algolia_top(&self, limit: usize) -> ApiResult<StoryPage> {
        let mut url = self.algolia_url("search")?;
        {
            let mut parameters = url.query_pairs_mut();
            parameters.append_pair("tags", "front_page");
            parameters.append_pair("hitsPerPage", &limit.to_string());
        }

        let response: AlgoliaSearchResponse = self.get_json(url).await?;
        let items: Vec<_> = response
            .hits
            .into_iter()
            .filter_map(AlgoliaHit::into_item)
            .take(limit)
            .collect();
        if items.is_empty() {
            return Err(ApiError::InvalidResponse(
                "Algolia returned an empty front page".to_owned(),
            ));
        }

        Ok(StoryPage {
            feed: Feed::Top,
            query: None,
            items,
            source: Source::Algolia,
            stale: false,
            fetched_at: unix_timestamp(),
        })
    }

    async fn hybrid_top(&self, limit: usize) -> ApiResult<StoryPage> {
        let (algolia, firebase_ids) = tokio::join!(
            self.algolia_top(limit),
            self.firebase_feed_ids(Feed::Top, limit)
        );

        match (algolia, firebase_ids) {
            (Ok(algolia_page), Ok(ids)) if !ids.is_empty() => {
                self.reconcile_top(algolia_page, ids).await
            }
            (Ok(algolia_page), Ok(_) | Err(_)) => Ok(algolia_page),
            (Err(_), Ok(ids)) if !ids.is_empty() => {
                self.firebase_page_from_ids(Feed::Top, ids, limit).await
            }
            (Err(primary), Ok(_)) => Err(primary),
            (Err(primary), Err(fallback)) => Err(ApiError::SourcesUnavailable {
                primary: Box::new(primary),
                fallback: Box::new(fallback),
            }),
        }
    }

    async fn reconcile_top(&self, algolia_page: StoryPage, ids: Vec<u64>) -> ApiResult<StoryPage> {
        let mut algolia_by_id = HashMap::with_capacity(algolia_page.items.len());
        for item in algolia_page.items {
            algolia_by_id.entry(item.id).or_insert(item);
        }

        let missing_ids = ids
            .iter()
            .copied()
            .filter(|id| !algolia_by_id.contains_key(id))
            .collect();
        let mut firebase_by_id: HashMap<_, _> = self
            .firebase_items_in_order(missing_ids)
            .await
            .into_iter()
            .map(|item| (item.id, item))
            .collect();

        let items: Vec<_> = ids
            .into_iter()
            .filter_map(|id| {
                algolia_by_id
                    .remove(&id)
                    .or_else(|| firebase_by_id.remove(&id))
            })
            .collect();
        if items.is_empty() {
            return Err(ApiError::InvalidResponse(
                "neither provider returned readable top stories".to_owned(),
            ));
        }

        Ok(StoryPage {
            feed: Feed::Top,
            query: None,
            items,
            source: Source::Hybrid,
            stale: false,
            fetched_at: unix_timestamp(),
        })
    }

    async fn firebase_feed(&self, feed: Feed, limit: usize) -> ApiResult<StoryPage> {
        let ids = self.firebase_feed_ids(feed, limit).await?;
        self.firebase_page_from_ids(feed, ids, limit).await
    }

    async fn firebase_feed_ids(&self, feed: Feed, limit: usize) -> ApiResult<Vec<u64>> {
        let url = self.firebase_url(feed.firebase_path())?;
        let mut ids: Vec<u64> = self.get_json(url).await?;
        ids.truncate(limit);
        Ok(ids)
    }

    async fn firebase_page_from_ids(
        &self,
        feed: Feed,
        ids: Vec<u64>,
        limit: usize,
    ) -> ApiResult<StoryPage> {
        let items = self.firebase_items_in_order(ids).await;
        if items.is_empty() && limit > 0 {
            return Err(ApiError::InvalidResponse(format!(
                "Firebase returned no readable items for the {feed} feed"
            )));
        }

        Ok(StoryPage {
            feed,
            query: None,
            items,
            source: Source::Firebase,
            stale: false,
            fetched_at: unix_timestamp(),
        })
    }

    async fn firebase_items_in_order(&self, ids: Vec<u64>) -> Vec<Item> {
        let mut results = stream::iter(ids.into_iter().enumerate())
            .map(|(index, id)| async move {
                match self.firebase_item(id).await {
                    Ok(item) => Some((index, item)),
                    Err(error) => {
                        tracing::debug!(item_id = id, %error, "skipping unreadable Firebase item");
                        None
                    }
                }
            })
            .buffer_unordered(self.max_concurrency)
            .filter_map(async move |result| result)
            .collect::<Vec<_>>()
            .await;
        results.sort_unstable_by_key(|(index, _)| *index);
        results.into_iter().map(|(_, item)| item).collect()
    }

    async fn algolia_item(&self, id: u64) -> ApiResult<Item> {
        let raw = self.algolia_item_raw(id).await?;
        let item = raw.into_item().ok_or_else(|| {
            ApiError::InvalidResponse(format!("Algolia item {id} has no valid id"))
        })?;
        if item.id != id {
            return Err(ApiError::InvalidResponse(format!(
                "Algolia returned item {} when item {id} was requested",
                item.id
            )));
        }
        Ok(item)
    }

    async fn algolia_item_raw(&self, id: u64) -> ApiResult<AlgoliaItem> {
        let url = self.algolia_url(&format!("items/{id}"))?;
        self.get_json(url).await
    }

    async fn firebase_item(&self, id: u64) -> ApiResult<Item> {
        let raw = self.firebase_item_raw(id).await?;
        let part_ids = raw.parts.clone();
        let mut item = raw.into_item();
        item.poll_options = self
            .firebase_poll_options(&part_ids)
            .await
            .into_boxed_slice();
        Ok(item)
    }

    async fn firebase_item_raw(&self, id: u64) -> ApiResult<FirebaseItem> {
        let url = self.firebase_url(&format!("item/{id}.json"))?;
        let item: Option<FirebaseItem> = self.get_json(url).await?;
        match item {
            Some(item) if item.id == id => Ok(item),
            Some(item) => Err(ApiError::InvalidResponse(format!(
                "Firebase returned item {} when item {id} was requested",
                item.id
            ))),
            None => Err(ApiError::NotFound(id)),
        }
    }

    async fn algolia_thread(&self, id: u64) -> ApiResult<Thread> {
        let raw = self.algolia_item_raw(id).await?;
        let (item, children) = raw.into_item_and_children().ok_or_else(|| {
            ApiError::InvalidResponse(format!("Algolia thread {id} has no valid root item"))
        })?;
        if item.id != id {
            return Err(ApiError::InvalidResponse(format!(
                "Algolia returned thread {} when thread {id} was requested",
                item.id
            )));
        }
        let comments = children
            .into_iter()
            .filter_map(|child| algolia_comment(child, 0, self.max_thread_depth))
            .collect();

        Ok(Thread {
            item,
            comments,
            source: Source::Algolia,
            stale: false,
            fetched_at: unix_timestamp(),
        })
    }

    async fn firebase_thread(&self, id: u64) -> ApiResult<Thread> {
        let deadline = Instant::now() + self.thread_timeout;
        let raw = timeout_at(deadline, self.firebase_item_raw(id))
            .await
            .map_err(|_| ApiError::Timeout {
                operation: "Firebase thread fetch",
            })??;
        let root_id = raw.id;
        let child_ids = raw.kids.clone();
        let part_ids = raw.parts.clone();
        let mut item = raw.into_item();
        item.poll_options = timeout_at(deadline, self.firebase_poll_options(&part_ids))
            .await
            .unwrap_or_default()
            .into_boxed_slice();
        let loaded = self
            .firebase_comment_records(&child_ids, root_id, deadline)
            .await;
        let comments = build_firebase_comments(root_id, &child_ids, loaded);

        Ok(Thread {
            item,
            comments,
            source: Source::Firebase,
            stale: false,
            fetched_at: unix_timestamp(),
        })
    }

    async fn firebase_comment_records(
        &self,
        root_ids: &[u64],
        root_id: u64,
        deadline: Instant,
    ) -> Vec<LoadedFirebaseComment> {
        let mut pending = VecDeque::new();
        for id in root_ids.iter().copied().take(self.max_thread_nodes) {
            pending.push_back(PendingFirebaseComment {
                id,
                owner: root_id,
                depth: 0,
            });
        }

        let mut visited = HashSet::from([root_id]);
        let mut scheduled = 0_usize;
        let mut loaded = Vec::new();

        'traversal: while !pending.is_empty() && scheduled < self.max_thread_nodes {
            let mut batch = Vec::with_capacity(self.max_concurrency);
            while batch.len() < self.max_concurrency && scheduled < self.max_thread_nodes {
                let Some(comment) = pending.pop_front() else {
                    break;
                };
                if comment.depth > self.max_thread_depth || !visited.insert(comment.id) {
                    continue;
                }
                scheduled = scheduled.saturating_add(1);
                batch.push(comment);
            }

            if batch.is_empty() {
                continue;
            }

            let mut requests = stream::iter(batch)
                .map(|comment| async move {
                    let result = self.firebase_item_raw(comment.id).await;
                    (comment, result)
                })
                .buffer_unordered(self.max_concurrency);

            loop {
                match timeout_at(deadline, requests.next()).await {
                    Ok(Some((comment, Ok(raw)))) => {
                        for child_id in raw.kids.iter().copied() {
                            if pending.len() >= self.max_thread_nodes {
                                break;
                            }
                            pending.push_back(PendingFirebaseComment {
                                id: child_id,
                                owner: raw.id,
                                depth: comment.depth.saturating_add(1),
                            });
                        }
                        loaded.push(LoadedFirebaseComment {
                            raw,
                            owner: comment.owner,
                            depth: comment.depth,
                        });
                    }
                    Ok(Some((comment, Err(error)))) => {
                        tracing::debug!(comment_id = comment.id, %error, "skipping unreadable Firebase comment");
                    }
                    Ok(None) => break,
                    Err(_) => {
                        tracing::debug!("Firebase comment traversal reached its overall timeout");
                        break 'traversal;
                    }
                }
            }
        }

        loaded
    }

    async fn firebase_poll_options(&self, ids: &[u64]) -> Vec<PollOption> {
        let mut options = stream::iter(ids.iter().copied().enumerate())
            .map(|(index, id)| async move {
                match self.firebase_item_raw(id).await {
                    Ok(raw) => raw.into_poll_option().map(|option| (index, option)),
                    Err(error) => {
                        tracing::debug!(poll_option_id = id, %error, "skipping unreadable Firebase poll option");
                        None
                    }
                }
            })
            .buffer_unordered(self.max_concurrency)
            .filter_map(async move |option| option)
            .collect::<Vec<_>>()
            .await;
        options.sort_unstable_by_key(|(index, _)| *index);
        options.into_iter().map(|(_, option)| option).collect()
    }

    async fn get_json<T: DeserializeOwned>(&self, url: Url) -> ApiResult<T> {
        let _permit = self
            .gate
            .acquire()
            .await
            .map_err(|_| ApiError::InvalidResponse("request limiter was closed".to_owned()))?;
        let response = self.http.get(url).send().await?.error_for_status()?;
        if response
            .content_length()
            .is_some_and(|length| length > self.max_response_bytes as u64)
        {
            return Err(ApiError::BodyTooLarge {
                limit: self.max_response_bytes,
            });
        }

        let mut body = Vec::new();
        let mut chunks = response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk?;
            if body.len().saturating_add(chunk.len()) > self.max_response_bytes {
                return Err(ApiError::BodyTooLarge {
                    limit: self.max_response_bytes,
                });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(serde_json::from_slice(&body)?)
    }

    fn algolia_url(&self, path: &str) -> ApiResult<Url> {
        Ok(self.algolia_base.join(path)?)
    }

    fn firebase_url(&self, path: &str) -> ApiResult<Url> {
        Ok(self.firebase_base.join(path)?)
    }
}

#[derive(Debug, Clone, Copy)]
struct PendingFirebaseComment {
    id: u64,
    owner: u64,
    depth: u32,
}

#[derive(Debug)]
struct LoadedFirebaseComment {
    raw: FirebaseItem,
    owner: u64,
    depth: u32,
}

fn build_firebase_comments(
    root_id: u64,
    root_ids: &[u64],
    mut records: Vec<LoadedFirebaseComment>,
) -> Vec<Comment> {
    records.sort_unstable_by(|left, right| {
        right
            .depth
            .cmp(&left.depth)
            .then_with(|| left.raw.id.cmp(&right.raw.id))
    });
    let owners: HashMap<_, _> = records
        .iter()
        .map(|record| (record.raw.id, record.owner))
        .collect();
    let mut built = HashMap::with_capacity(records.len());

    for record in records {
        let id = record.raw.id;
        let children = record
            .raw
            .kids
            .iter()
            .filter_map(|child_id| {
                (owners.get(child_id) == Some(&id))
                    .then(|| built.remove(child_id))
                    .flatten()
            })
            .collect();
        let comment = record
            .raw
            .into_comment(record.depth, record.owner, children);
        built.insert(id, comment);
    }

    root_ids
        .iter()
        .filter_map(|id| {
            (owners.get(id) == Some(&root_id))
                .then(|| built.remove(id))
                .flatten()
        })
        .collect()
}

fn more_complete_thread(algolia: Thread, firebase: Thread) -> Thread {
    let algolia_metadata = algolia.metadata();
    let firebase_metadata = firebase.metadata();

    if algolia_metadata.partial != firebase_metadata.partial {
        return if firebase_metadata.partial {
            algolia
        } else {
            firebase
        };
    }
    if algolia_metadata.omitted_comments != firebase_metadata.omitted_comments {
        return if firebase_metadata.omitted_comments < algolia_metadata.omitted_comments {
            firebase
        } else {
            algolia
        };
    }
    if firebase_metadata.loaded_comments < algolia_metadata.loaded_comments {
        algolia
    } else {
        // Firebase is canonical and wins equal completeness.
        firebase
    }
}

impl DataSource for HybridClient {
    fn feed(&self, feed: Feed, limit: usize) -> BoxFuture<'_, ApiResult<StoryPage>> {
        Box::pin(HybridClient::feed(self, feed, limit))
    }

    fn item(&self, id: u64) -> BoxFuture<'_, ApiResult<Item>> {
        Box::pin(HybridClient::item(self, id))
    }

    fn thread(&self, id: u64) -> BoxFuture<'_, ApiResult<Thread>> {
        Box::pin(HybridClient::thread(self, id))
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        search_type: SearchType,
        limit: usize,
    ) -> BoxFuture<'a, ApiResult<StoryPage>> {
        Box::pin(HybridClient::search(self, query, search_type, limit))
    }
}

fn parse_base_url(value: &str) -> ApiResult<Url> {
    let mut url = Url::parse(value)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ApiError::InvalidBaseUrl(format!(
            "unsupported `{}` scheme; expected http or https",
            url.scheme()
        )));
    }
    if url.host_str().is_none() || url.cannot_be_a_base() {
        return Err(ApiError::InvalidBaseUrl(
            "URL must contain a host and support relative paths".to_owned(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ApiError::InvalidBaseUrl(
            "embedded credentials are not allowed".to_owned(),
        ));
    }
    url.set_query(None);
    url.set_fragment(None);
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

const fn bounded_limit(limit: usize) -> usize {
    if limit > MAX_RESULTS {
        MAX_RESULTS
    } else {
        limit
    }
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

fn empty_page(feed: Feed, source: Source) -> StoryPage {
    StoryPage {
        feed,
        query: None,
        items: Vec::new(),
        source,
        stale: false,
        fetched_at: unix_timestamp(),
    }
}

#[derive(Debug, Deserialize)]
struct AlgoliaSearchResponse {
    #[serde(default)]
    hits: Vec<AlgoliaHit>,
}

#[derive(Debug, Deserialize)]
struct AlgoliaHit {
    #[serde(rename = "objectID")]
    object_id: Option<StringOrNumber>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    story_title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    story_url: Option<String>,
    #[serde(default)]
    story_text: Option<String>,
    #[serde(default)]
    comment_text: Option<String>,
    #[serde(default)]
    created_at_i: Option<i64>,
    #[serde(default)]
    points: Option<i64>,
    #[serde(default)]
    num_comments: Option<u64>,
    #[serde(default)]
    parent_id: Option<u64>,
    #[serde(default, rename = "_tags")]
    tags: Vec<String>,
}

impl AlgoliaHit {
    fn into_item(self) -> Option<Item> {
        let id = self.object_id?.into_u64()?;
        let item_type = if self.tags.iter().any(|tag| tag == "comment") {
            "comment"
        } else if self.tags.iter().any(|tag| tag == "job") {
            "job"
        } else if self.tags.iter().any(|tag| tag == "pollopt") {
            "pollopt"
        } else if self.tags.iter().any(|tag| tag == "poll") {
            "poll"
        } else {
            "story"
        };

        Some(Item {
            id,
            by: nonempty(self.author),
            title: nonempty(self.title.or(self.story_title)),
            url: nonempty(self.url.or(self.story_url)),
            text: nonempty(self.comment_text.or(self.story_text)),
            time: self.created_at_i.unwrap_or_default(),
            score: self.points.unwrap_or_default(),
            descendants: self.num_comments.unwrap_or_default(),
            kids: Vec::new(),
            parts: Box::default(),
            poll_options: Box::default(),
            parent: self.parent_id,
            deleted: false,
            dead: false,
            item_type: item_type.to_owned(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct AlgoliaItem {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    created_at_i: Option<i64>,
    #[serde(default)]
    points: Option<i64>,
    #[serde(default)]
    num_comments: Option<u64>,
    #[serde(default)]
    descendants: Option<u64>,
    #[serde(default, rename = "type")]
    item_type: Option<String>,
    #[serde(default)]
    parent_id: Option<u64>,
    #[serde(default)]
    children: Vec<Self>,
    #[serde(default)]
    options: Vec<Self>,
}

impl AlgoliaItem {
    fn into_item(self) -> Option<Item> {
        self.into_item_and_children().map(|(item, _)| item)
    }

    fn into_item_and_children(self) -> Option<(Item, Vec<Self>)> {
        let Self {
            id,
            author,
            title,
            url,
            text,
            created_at_i,
            points,
            num_comments,
            descendants: declared_descendants,
            item_type,
            parent_id,
            children,
            options,
        } = self;
        let id = id.filter(|id| *id > 0)?;
        let loaded_descendants = count_algolia_descendants(&children);
        let descendants = num_comments
            .or(declared_descendants)
            .unwrap_or(loaded_descendants)
            .max(loaded_descendants);
        let kids = children.iter().filter_map(|child| child.id).collect();
        let parts = options.iter().filter_map(|option| option.id).collect();
        let poll_options = options
            .into_iter()
            .filter_map(Self::into_poll_option)
            .collect();
        let item = Item {
            id,
            by: nonempty(author),
            title: nonempty(title),
            url: nonempty(url),
            text: nonempty(text),
            time: created_at_i.unwrap_or_default(),
            score: points.unwrap_or_default(),
            descendants,
            kids,
            parts,
            poll_options,
            parent: parent_id,
            deleted: false,
            dead: false,
            item_type: item_type.unwrap_or_else(|| "story".to_owned()),
        };
        Some((item, children))
    }

    fn into_poll_option(self) -> Option<PollOption> {
        Some(PollOption {
            id: self.id.filter(|id| *id > 0)?,
            by: nonempty(self.author),
            text: nonempty(self.text.or(self.title)),
            time: self.created_at_i.unwrap_or_default(),
            score: self.points.unwrap_or_default(),
            parent: self.parent_id,
            deleted: false,
            dead: false,
        })
    }
}

/// Converts one Algolia record and its nested children into a [`Comment`].
///
/// `max_depth` bounds the recursion exactly as the Firebase traversal does.
/// Beyond it the node is dropped while its id stays in the parent's `kids`, so
/// [`Thread::metadata`] reports the tree as partial with the omitted ids listed
/// rather than silently returning a truncated tree.
fn algolia_comment(raw: AlgoliaItem, depth: u32, max_depth: u32) -> Option<Comment> {
    if depth > max_depth {
        return None;
    }
    let id = raw.id.filter(|id| *id > 0)?;
    let kids = raw.children.iter().filter_map(|child| child.id).collect();
    let children = raw
        .children
        .into_iter()
        .filter_map(|child| algolia_comment(child, depth.saturating_add(1), max_depth))
        .collect();
    Some(Comment {
        id,
        by: nonempty(raw.author),
        text: nonempty(raw.text),
        time: raw.created_at_i.unwrap_or_default(),
        parent: raw.parent_id,
        kids,
        children,
        deleted: false,
        dead: false,
        depth,
    })
}

fn count_algolia_descendants(children: &[AlgoliaItem]) -> u64 {
    let mut count = 0_u64;
    let mut pending: Vec<_> = children.iter().collect();
    while let Some(child) = pending.pop() {
        count = count.saturating_add(1);
        pending.extend(child.children.iter());
    }
    count
}

#[derive(Debug, Deserialize)]
struct FirebaseItem {
    id: u64,
    #[serde(default)]
    by: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    time: i64,
    #[serde(default)]
    score: i64,
    #[serde(default)]
    descendants: u64,
    #[serde(default)]
    kids: Vec<u64>,
    #[serde(default)]
    parts: Vec<u64>,
    #[serde(default)]
    deleted: bool,
    #[serde(default)]
    dead: bool,
    #[serde(default, rename = "type")]
    item_type: String,
    #[serde(default)]
    parent: Option<u64>,
}

impl FirebaseItem {
    fn into_item(self) -> Item {
        Item {
            id: self.id,
            by: nonempty(self.by),
            title: nonempty(self.title),
            url: nonempty(self.url),
            text: nonempty(self.text),
            time: self.time,
            score: self.score,
            descendants: self.descendants,
            kids: self.kids,
            parts: self.parts.into_boxed_slice(),
            poll_options: Box::default(),
            parent: self.parent,
            deleted: self.deleted,
            dead: self.dead,
            item_type: self.item_type,
        }
    }

    fn into_comment(self, depth: u32, owner: u64, children: Vec<Comment>) -> Comment {
        Comment {
            id: self.id,
            by: nonempty(self.by),
            text: nonempty(self.text),
            time: self.time,
            parent: self.parent.or(Some(owner)),
            kids: self.kids,
            children,
            deleted: self.deleted,
            dead: self.dead,
            depth,
        }
    }

    fn into_poll_option(self) -> Option<PollOption> {
        if self.item_type != "pollopt" {
            return None;
        }
        Some(PollOption {
            id: self.id,
            by: nonempty(self.by),
            text: nonempty(self.text.or(self.title)),
            time: self.time,
            score: self.score,
            parent: self.parent,
            deleted: self.deleted,
            dead: self.dead,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StringOrNumber {
    String(String),
    Number(u64),
}

impl StringOrNumber {
    fn into_u64(self) -> Option<u64> {
        match self {
            Self::String(value) => value.parse().ok().filter(|id| *id > 0),
            Self::Number(value) => (value > 0).then_some(value),
        }
    }
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use wiremock::{
        Mock, MockServer, Request, ResponseTemplate,
        matchers::{method, path, path_regex},
    };

    use super::{AlgoliaHit, ApiError, Feed, HybridClient, SearchType, Source};

    #[test]
    fn algolia_hit_maps_comment_fields() {
        let hit: AlgoliaHit = serde_json::from_value(serde_json::json!({
            "objectID": "123",
            "author": "alice",
            "story_title": "Parent story",
            "story_url": "https://example.com",
            "comment_text": "hello",
            "created_at_i": 99,
            "_tags": ["comment", "author_alice"]
        }))
        .expect("hit parses");
        let item = hit.into_item().expect("hit has an id");

        assert_eq!(item.id, 123);
        assert_eq!(item.title.as_deref(), Some("Parent story"));
        assert_eq!(item.text.as_deref(), Some("hello"));
        assert_eq!(item.item_type, "comment");
    }

    #[tokio::test]
    async fn top_feed_falls_back_and_keeps_firebase_order() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/topstories.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec![3, 1, 2]))
            .mount(&server)
            .await;
        for (id, delay) in [(1_u64, 20_u64), (2, 5), (3, 40)] {
            Mock::given(method("GET"))
                .and(path(format!("/item/{id}.json")))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_delay(Duration::from_millis(delay))
                        .set_body_json(serde_json::json!({
                            "id": id,
                            "type": "story",
                            "title": format!("Story {id}")
                        })),
                )
                .mount(&server)
                .await;
        }

        let client = HybridClient::with_base_urls(server.uri(), server.uri())
            .expect("test URLs are valid")
            .with_max_concurrency(3);
        let page = client.feed(Feed::Top, 3).await.expect("fallback succeeds");

        assert_eq!(page.source, Source::Firebase);
        assert_eq!(
            page.items.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![3, 1, 2]
        );
    }

    #[tokio::test]
    async fn algolia_search_preserves_ranked_hit_order() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "hits": [
                    {"objectID": "9", "title": "first", "_tags": ["story"]},
                    {"objectID": "4", "title": "second", "_tags": ["story"]}
                ]
            })))
            .mount(&server)
            .await;

        let client =
            HybridClient::with_base_urls(server.uri(), server.uri()).expect("test URLs are valid");
        let page = client
            .search("rust", SearchType::Story, 2)
            .await
            .expect("search succeeds");

        assert_eq!(page.query.as_deref(), Some("rust"));
        assert_eq!(
            page.items.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![9, 4]
        );
    }

    #[tokio::test]
    async fn thread_fallback_builds_a_deterministic_recursive_tree() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/items/100"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/item/100.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 100,
                "type": "story",
                "title": "root",
                "kids": [30, 20]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/item/30.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(30))
                    .set_body_json(serde_json::json!({
                        "id": 30,
                        "type": "comment",
                        "parent": 100,
                        "text": "first",
                        "kids": [31]
                    })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/item/20.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(5))
                    .set_body_json(serde_json::json!({
                        "id": 20,
                        "type": "comment",
                        "parent": 100,
                        "text": "second"
                    })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/item/31.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 31,
                "type": "comment",
                "parent": 30,
                "text": "child"
            })))
            .mount(&server)
            .await;

        let client = HybridClient::with_base_urls(server.uri(), server.uri())
            .expect("test URLs are valid")
            .with_max_concurrency(20);
        let thread = client.thread(100).await.expect("fallback succeeds");

        assert_eq!(client.max_concurrency(), 12);
        assert_eq!(thread.source, Source::Firebase);
        assert_eq!(
            thread
                .comments
                .iter()
                .map(|comment| comment.id)
                .collect::<Vec<_>>(),
            vec![30, 20]
        );
        assert_eq!(thread.comments[0].children[0].id, 31);
        assert_eq!(thread.comments[0].children[0].depth, 1);
    }

    #[tokio::test]
    async fn top_reconciliation_uses_firebase_order_and_fetches_only_missing_items() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "hits": [
                    {"objectID": "2", "title": "two", "_tags": ["story"]},
                    {"objectID": "1", "title": "one", "_tags": ["story"]}
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/topstories.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec![1, 2, 3]))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/item/3.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 3,
                "type": "story",
                "title": "three"
            })))
            .mount(&server)
            .await;

        let client =
            HybridClient::with_base_urls(server.uri(), server.uri()).expect("test URLs are valid");
        let page = client.feed(Feed::Top, 3).await.expect("top feed loads");

        assert_eq!(page.source, Source::Hybrid);
        assert_eq!(
            page.items.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        let requests = server
            .received_requests()
            .await
            .expect("request recording is enabled");
        let item_paths: Vec<_> = requests
            .iter()
            .map(|request| request.url.path())
            .filter(|path| path.starts_with("/item/"))
            .collect();
        assert_eq!(item_paths, vec!["/item/3.json"]);
    }

    #[tokio::test]
    async fn authoritative_firebase_null_is_reported_as_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/items/7"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/item/7.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::Value::Null))
            .mount(&server)
            .await;

        let client =
            HybridClient::with_base_urls(server.uri(), server.uri()).expect("test URLs are valid");
        let error = client
            .item_with_source(7)
            .await
            .expect_err("null is not found");

        assert!(matches!(error, ApiError::NotFound(7)));
    }

    #[tokio::test]
    async fn firebase_cycles_are_fetched_once_and_marked_partial() {
        let server = MockServer::start().await;
        mount_algolia_thread_failure(&server, 200).await;
        mount_firebase_item(
            &server,
            200,
            serde_json::json!({
                "id": 200, "type": "story", "descendants": 3, "kids": [201]
            }),
        )
        .await;
        mount_firebase_item(
            &server,
            201,
            serde_json::json!({
                "id": 201, "type": "comment", "parent": 200, "kids": [202]
            }),
        )
        .await;
        mount_firebase_item(
            &server,
            202,
            serde_json::json!({
                "id": 202, "type": "comment", "parent": 201, "kids": [201]
            }),
        )
        .await;

        let client =
            HybridClient::with_base_urls(server.uri(), server.uri()).expect("test URLs are valid");
        let thread = client.thread(200).await.expect("partial thread loads");
        let metadata = thread.metadata();

        assert_eq!(thread.comment_count(), 2);
        assert!(metadata.partial);
        assert_eq!(metadata.unresolved_ids, vec![201]);
        let requests = server
            .received_requests()
            .await
            .expect("request recording is enabled");
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.url.path() == "/item/201.json")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn firebase_depth_cap_returns_explicit_partial_metadata() {
        let server = MockServer::start().await;
        mount_algolia_thread_failure(&server, 300).await;
        mount_firebase_item(
            &server,
            300,
            serde_json::json!({
                "id": 300, "type": "story", "descendants": 3, "kids": [301]
            }),
        )
        .await;
        mount_firebase_item(
            &server,
            301,
            serde_json::json!({
                "id": 301, "type": "comment", "kids": [302]
            }),
        )
        .await;
        mount_firebase_item(
            &server,
            302,
            serde_json::json!({
                "id": 302, "type": "comment", "kids": [303]
            }),
        )
        .await;

        let client = HybridClient::with_base_urls(server.uri(), server.uri())
            .expect("test URLs are valid")
            .with_thread_limits(100, 1, Duration::from_secs(1));
        let thread = client.thread(300).await.expect("bounded thread loads");

        assert_eq!(thread.comment_count(), 2);
        assert_eq!(thread.comments[0].children[0].depth, 1);
        assert_eq!(thread.metadata().unresolved_ids, vec![303]);
    }

    #[tokio::test]
    async fn firebase_thread_timeout_returns_the_loaded_partial_tree() {
        let server = MockServer::start().await;
        mount_algolia_thread_failure(&server, 400).await;
        mount_firebase_item(
            &server,
            400,
            serde_json::json!({
                "id": 400, "type": "story", "descendants": 1, "kids": [401]
            }),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/item/401.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(100))
                    .set_body_json(serde_json::json!({
                        "id": 401, "type": "comment", "parent": 400
                    })),
            )
            .mount(&server)
            .await;

        let client = HybridClient::with_base_urls(server.uri(), server.uri())
            .expect("test URLs are valid")
            .with_thread_limits(100, 10, Duration::from_millis(25));
        let thread = client.thread(400).await.expect("partial thread loads");

        assert!(thread.comments.is_empty());
        assert_eq!(thread.metadata().unresolved_ids, vec![401]);
    }

    #[tokio::test]
    async fn decompressed_response_body_is_capped() {
        const COMPRESSED_JSON: &[u8] = &[
            0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xab, 0x56, 0xca, 0xc8,
            0x2c, 0x29, 0x56, 0xb2, 0x8a, 0x8e, 0xd5, 0x51, 0x2a, 0x48, 0x4c, 0x49, 0xc9, 0xcc,
            0x4b, 0x57, 0xb2, 0x52, 0xaa, 0x18, 0xe1, 0x40, 0xa9, 0x16, 0x00, 0xc5, 0xa4, 0xf2,
            0x0c, 0x18, 0x01, 0x00, 0x00,
        ];
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-encoding", "gzip")
                    .insert_header("content-type", "application/json")
                    .set_body_bytes(COMPRESSED_JSON),
            )
            .mount(&server)
            .await;

        let client = HybridClient::with_base_urls(server.uri(), server.uri())
            .expect("test URLs are valid")
            .with_max_response_bytes(64);
        let error = client
            .search("bounded", SearchType::Story, 1)
            .await
            .expect_err("expanded body exceeds cap");

        assert!(matches!(error, ApiError::BodyTooLarge { limit: 64 }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn firebase_fanout_never_exceeds_configured_concurrency() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/newstories.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec![1, 2, 3, 4, 5, 6]))
            .mount(&server)
            .await;

        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let responder_active = Arc::clone(&active);
        let responder_peak = Arc::clone(&peak);
        Mock::given(method("GET"))
            .and(path_regex(r"^/item/\d+\.json$"))
            .respond_with(move |request: &Request| {
                let current = responder_active.fetch_add(1, Ordering::SeqCst) + 1;
                responder_peak.fetch_max(current, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(20));
                responder_active.fetch_sub(1, Ordering::SeqCst);
                let id = request
                    .url
                    .path_segments()
                    .and_then(|mut segments| segments.nth(1))
                    .and_then(|segment| segment.strip_suffix(".json"))
                    .and_then(|segment| segment.parse::<u64>().ok())
                    .expect("matched item path contains an id");
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": id, "type": "story"
                }))
            })
            .mount(&server)
            .await;

        let client = HybridClient::with_base_urls(server.uri(), server.uri())
            .expect("test URLs are valid")
            .with_max_concurrency(3);
        let page = client.feed(Feed::New, 6).await.expect("feed loads");

        assert_eq!(page.len(), 6);
        assert!(peak.load(Ordering::SeqCst) <= 3);
    }

    #[tokio::test]
    async fn poll_fields_and_provider_are_preserved() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/items/500"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        mount_firebase_item(
            &server,
            500,
            serde_json::json!({
                "id": 500, "type": "poll", "title": "choose", "parts": [501]
            }),
        )
        .await;
        mount_firebase_item(
            &server,
            501,
            serde_json::json!({
                "id": 501, "type": "pollopt", "parent": 500, "text": "option", "score": 9
            }),
        )
        .await;

        let client =
            HybridClient::with_base_urls(server.uri(), server.uri()).expect("test URLs are valid");
        let (item, source) = client
            .item_with_source(500)
            .await
            .expect("Firebase poll loads");

        assert_eq!(source, Source::Firebase);
        assert_eq!(item.parts.as_ref(), &[501]);
        assert_eq!(item.poll_options[0].id, 501);
        assert_eq!(item.poll_options[0].parent, Some(500));
    }

    #[tokio::test]
    async fn partial_algolia_thread_is_replaced_by_complete_firebase_tree() {
        let server = MockServer::start().await;
        mount_partial_algolia_thread(&server, 700, 701).await;
        mount_firebase_item(
            &server,
            700,
            serde_json::json!({
                "id": 700,
                "type": "story",
                "title": "canonical",
                "descendants": 2,
                "kids": [701, 702]
            }),
        )
        .await;
        mount_firebase_item(
            &server,
            701,
            serde_json::json!({
                "id": 701, "type": "comment", "parent": 700, "text": "first"
            }),
        )
        .await;
        mount_firebase_item(
            &server,
            702,
            serde_json::json!({
                "id": 702, "type": "comment", "parent": 700, "text": "second"
            }),
        )
        .await;

        let client =
            HybridClient::with_base_urls(server.uri(), server.uri()).expect("test URLs are valid");
        let thread = client.thread(700).await.expect("thread loads");

        assert_eq!(thread.source, Source::Firebase);
        assert!(!thread.is_partial());
        assert_eq!(
            thread
                .comments
                .iter()
                .map(|comment| comment.id)
                .collect::<Vec<_>>(),
            vec![701, 702]
        );
    }

    #[tokio::test]
    async fn algolia_depth_cap_matches_firebase_and_reports_partial() {
        let server = MockServer::start().await;
        // Algolia returns a three-deep chain in one response; the depth cap has
        // to bound the recursive conversion, not just the Firebase fan-out.
        Mock::given(method("GET"))
            .and(path("/items/900"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 900,
                "type": "story",
                "title": "nested",
                "num_comments": 3,
                "children": [{
                    "id": 901,
                    "type": "comment",
                    "parent_id": 900,
                    "children": [{
                        "id": 902,
                        "type": "comment",
                        "parent_id": 901,
                        "children": [{
                            "id": 903,
                            "type": "comment",
                            "parent_id": 902
                        }]
                    }]
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/item/900.json"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let client = HybridClient::with_base_urls(server.uri(), server.uri())
            .expect("test URLs are valid")
            .with_thread_limits(100, 1, Duration::from_secs(1));
        let thread = client.thread(900).await.expect("bounded thread loads");

        // Same shape the Firebase depth-cap test asserts: two nodes kept, the
        // over-depth id left unresolved rather than silently dropped.
        assert_eq!(thread.comment_count(), 2);
        assert_eq!(thread.comments[0].children[0].depth, 1);
        assert!(thread.is_partial());
        assert_eq!(thread.metadata().unresolved_ids, vec![903]);
    }

    #[tokio::test]
    async fn partial_algolia_thread_remains_usable_when_firebase_fails() {
        let server = MockServer::start().await;
        mount_partial_algolia_thread(&server, 800, 801).await;
        Mock::given(method("GET"))
            .and(path("/item/800.json"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let client =
            HybridClient::with_base_urls(server.uri(), server.uri()).expect("test URLs are valid");
        let thread = client
            .thread(800)
            .await
            .expect("partial Algolia thread remains usable");

        assert_eq!(thread.source, Source::Algolia);
        assert!(thread.is_partial());
        assert_eq!(thread.comment_count(), 1);
        assert_eq!(thread.comments[0].id, 801);
    }

    #[tokio::test]
    async fn complete_algolia_thread_does_not_request_firebase() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/items/850"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 850,
                "type": "story",
                "num_comments": 1,
                "children": [{"id": 851, "type": "comment", "parent_id": 850}]
            })))
            .mount(&server)
            .await;

        let client =
            HybridClient::with_base_urls(server.uri(), server.uri()).expect("test URLs are valid");
        let thread = client.thread(850).await.expect("complete thread loads");

        assert_eq!(thread.source, Source::Algolia);
        assert!(!thread.is_partial());
        let requests = server
            .received_requests()
            .await
            .expect("request recording is enabled");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url.path(), "/items/850");
    }

    #[tokio::test]
    async fn canonical_firebase_wins_equal_partial_completeness() {
        let server = MockServer::start().await;
        mount_partial_algolia_thread(&server, 900, 901).await;
        mount_firebase_item(
            &server,
            900,
            serde_json::json!({
                "id": 900, "type": "story", "descendants": 2, "kids": [901, 902]
            }),
        )
        .await;
        mount_firebase_item(
            &server,
            901,
            serde_json::json!({
                "id": 901, "type": "comment", "parent": 900, "text": "canonical"
            }),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/item/902.json"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let client =
            HybridClient::with_base_urls(server.uri(), server.uri()).expect("test URLs are valid");
        let thread = client.thread(900).await.expect("partial thread loads");

        assert_eq!(thread.source, Source::Firebase);
        assert!(thread.is_partial());
        assert_eq!(thread.comment_count(), 1);
    }

    #[test]
    fn base_urls_require_http_hosts_without_credentials() {
        for invalid in [
            "file:///tmp/api",
            "mailto:api@example.com",
            "http://user:secret@example.com/api",
        ] {
            assert!(HybridClient::with_base_urls(invalid, "https://example.com").is_err());
        }
    }

    async fn mount_algolia_thread_failure(server: &MockServer, id: u64) {
        Mock::given(method("GET"))
            .and(path(format!("/items/{id}")))
            .respond_with(ResponseTemplate::new(503))
            .mount(server)
            .await;
    }

    async fn mount_partial_algolia_thread(server: &MockServer, id: u64, comment_id: u64) {
        Mock::given(method("GET"))
            .and(path(format!("/items/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": id,
                "type": "story",
                "title": "partial",
                "num_comments": 2,
                "children": [{
                    "id": comment_id,
                    "type": "comment",
                    "parent_id": id,
                    "text": "only indexed comment"
                }]
            })))
            .mount(server)
            .await;
    }

    async fn mount_firebase_item(server: &MockServer, id: u64, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path(format!("/item/{id}.json")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }
}
