//! Durable, cache-first storage backed by `SQLite`.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;

use crate::model::{Feed, Item, Source, StoryPage, Thread};

/// Current on-disk schema version.
pub const SCHEMA_VERSION: u32 = 5;
/// Conservative upper bound for one serialized cache value.
pub const MAX_CACHE_VALUE_BYTES: usize = 16 * 1024 * 1024;

const MAX_SEARCH_KEY_BYTES: usize = 4 * 1024;
const MAX_SETTING_KEY_BYTES: usize = 256;
const MAX_SETTING_VALUE_BYTES: usize = 1024 * 1024;

/// Errors produced by cache setup or access.
#[derive(Debug, Error)]
pub enum CacheError {
    #[error("SQLite cache error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("cache serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("cache filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("cache lock was poisoned")]
    LockPoisoned,
    #[error("invalid cache key: {0}")]
    InvalidKey(&'static str),
    #[error("cache value is too large ({actual} bytes; limit is {limit} bytes)")]
    ValueTooLarge { actual: usize, limit: usize },
    #[error("item ID {0} cannot be represented by SQLite")]
    InvalidItemId(u64),
    #[error("cache schema version {found} is newer than supported version {supported}")]
    UnsupportedSchema { found: u32, supported: u32 },
    #[error("the operating system did not provide a cache directory")]
    CacheDirectoryUnavailable,
}

pub type CacheResult<T> = Result<T, CacheError>;

#[derive(Clone, Copy)]
struct PageCounts {
    covered: usize,
    available: usize,
}

/// Timing information associated with a cached value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CacheMetadata {
    pub fetched_at: i64,
    pub expires_at: i64,
    pub stale: bool,
    /// Number of ordered items retained in a feed/search payload, when applicable.
    pub item_count: Option<usize>,
}

impl CacheMetadata {
    #[must_use]
    pub const fn is_fresh(self) -> bool {
        !self.stale
    }

    /// Whether this row is known to contain at least `requested` ordered items.
    #[must_use]
    pub const fn contains_at_least(self, requested: usize) -> bool {
        match self.item_count {
            Some(item_count) => item_count >= requested,
            None => false,
        }
    }
}

/// A cached value together with its freshness information.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheEntry<T> {
    pub value: T,
    pub metadata: CacheMetadata,
}

impl<T> CacheEntry<T> {
    #[must_use]
    pub const fn is_stale(&self) -> bool {
        self.metadata.stale
    }

    #[must_use]
    pub fn into_inner(self) -> T {
        self.value
    }

    /// Whether the persisted payload is known to satisfy an item limit.
    #[must_use]
    pub const fn contains_at_least(&self, requested: usize) -> bool {
        self.metadata.contains_at_least(requested)
    }
}

/// Row counts and size information for the cache database.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheStats {
    pub feeds: u64,
    pub items: u64,
    pub threads: u64,
    pub searches: u64,
    pub bookmarks: u64,
    pub read_items: u64,
    pub settings: u64,
    pub stale_entries: u64,
    pub payload_bytes: u64,
    pub database_bytes: u64,
}

impl CacheStats {
    #[must_use]
    pub const fn cache_entries(self) -> u64 {
        self.feeds + self.items + self.threads + self.searches
    }

    #[must_use]
    pub const fn total_rows(self) -> u64 {
        self.cache_entries() + self.bookmarks + self.read_items + self.settings
    }
}

/// Counts returned after pruning expired cache rows.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PruneStats {
    pub feeds: usize,
    pub items: usize,
    pub threads: usize,
    pub searches: usize,
}

impl PruneStats {
    #[must_use]
    pub const fn total(self) -> usize {
        self.feeds + self.items + self.threads + self.searches
    }
}

/// A thread-safe handle to one `SQLite` cache database.
///
/// All SQL values are parameterized. The connection is serialized behind a
/// mutex so a `Cache` can safely be shared between async application tasks.
#[derive(Clone, Debug)]
pub struct Cache {
    connection: Arc<Mutex<Connection>>,
    path: Option<PathBuf>,
}

// Cache operations share one documented error type, and each fallible method
// propagates only `CacheError` variants from validation, serialization, locking,
// filesystem, or SQLite access. Repeating that list on every CRUD method would
// obscure the API-specific documentation.
#[allow(clippy::missing_errors_doc)]
impl Cache {
    /// Opens (or creates) a cache at an explicit file path and migrates it.
    pub fn open(path: impl AsRef<Path>) -> CacheResult<Self> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return Err(CacheError::InvalidKey("cache path must not be empty"));
        }
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }

        let connection = Connection::open(path)?;
        Self::from_connection(connection, Some(path.to_path_buf()))
    }

    /// Opens `cache.sqlite3` below an explicit directory.
    pub fn open_in_dir(directory: impl AsRef<Path>) -> CacheResult<Self> {
        Self::open(directory.as_ref().join("cache.sqlite3"))
    }

    /// Returns the platform cache location used by [`Self::open_default`].
    pub fn default_path() -> CacheResult<PathBuf> {
        directories::ProjectDirs::from("com", "hnx", "hnx")
            .map(|directories| directories.cache_dir().join("cache.sqlite3"))
            .ok_or(CacheError::CacheDirectoryUnavailable)
    }

    /// Opens the cache at the platform-specific default path.
    pub fn open_default() -> CacheResult<Self> {
        Self::open(Self::default_path()?)
    }

    /// Creates a migrated in-memory cache for tests and short-lived commands.
    pub fn open_in_memory() -> CacheResult<Self> {
        let connection = Connection::open_in_memory()?;
        Self::from_connection(connection, None)
    }

    fn from_connection(mut connection: Connection, path: Option<PathBuf>) -> CacheResult<Self> {
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA temp_store = MEMORY;",
        )?;
        if path.is_some() {
            let _: String =
                connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
            connection.execute_batch("PRAGMA synchronous = NORMAL;")?;
        }
        migrate_connection(&mut connection)?;

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            path,
        })
    }

    /// The backing database path, or `None` for an in-memory cache.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Applies any pending migrations. Opening a cache already calls this.
    pub fn migrate(&self) -> CacheResult<()> {
        let mut connection = self.lock()?;
        migrate_connection(&mut connection)
    }

    /// Returns the `SQLite` user schema version.
    pub fn schema_version(&self) -> CacheResult<u32> {
        let version = self
            .lock()?
            .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?;
        Ok(version)
    }

    /// Stores a feed page, using its fetch timestamp when one is populated.
    pub fn put_feed(&self, page: &StoryPage, ttl: Duration) -> CacheResult<CacheMetadata> {
        let fetched_at = valid_or_current_timestamp(page.fetched_at);
        let payload = encode(page)?;
        let item_rows = encode_item_rows(&page.items)?;
        self.put_feed_encoded(
            page.feed,
            &payload,
            PageCounts {
                covered: page.covered_slots(),
                available: page.items.len(),
            },
            &item_rows,
            fetched_at,
            ttl,
        )
    }

    /// Reads a feed even when it has expired. Inspect `metadata.stale` before use.
    pub fn get_feed(&self, feed: Feed) -> CacheResult<Option<CacheEntry<StoryPage>>> {
        let mut entry = self.get_feed_value::<StoryPage>(feed)?;
        if let Some(cached) = &mut entry {
            cached.value.source = Source::Cache;
            cached.value.stale = cached.metadata.stale;
            cached.value.fetched_at = cached.metadata.fetched_at;
        }
        Ok(entry)
    }

    /// Reads a feed only if its TTL has not elapsed.
    pub fn get_fresh_feed(&self, feed: Feed) -> CacheResult<Option<StoryPage>> {
        Ok(self
            .get_feed(feed)?
            .filter(|entry| entry.metadata.is_fresh())
            .map(CacheEntry::into_inner))
    }

    /// Reads a feed only when its persisted page contains at least `requested` items.
    /// Freshness remains available through the returned `metadata` field.
    pub fn get_feed_for_limit(
        &self,
        feed: Feed,
        requested: usize,
    ) -> CacheResult<Option<CacheEntry<StoryPage>>> {
        Ok(self
            .get_feed(feed)?
            .filter(|entry| entry.contains_at_least(requested)))
    }

    /// Reads a feed only when it is fresh and contains at least `requested` items.
    pub fn get_fresh_feed_for_limit(
        &self,
        feed: Feed,
        requested: usize,
    ) -> CacheResult<Option<StoryPage>> {
        Ok(self
            .get_feed_for_limit(feed, requested)?
            .filter(|entry| entry.metadata.is_fresh())
            .map(CacheEntry::into_inner))
    }

    /// Generic feed storage used by tests and forward-compatible callers.
    pub fn put_feed_value<T: Serialize>(
        &self,
        feed: Feed,
        value: &T,
        fetched_at: i64,
        ttl: Duration,
    ) -> CacheResult<CacheMetadata> {
        let payload = encode(value)?;
        let counts = serialized_story_counts(&payload).unwrap_or(PageCounts {
            covered: 0,
            available: 0,
        });
        self.put_feed_encoded(feed, &payload, counts, &[], fetched_at, ttl)
    }

    fn put_feed_encoded(
        &self,
        feed: Feed,
        payload: &[u8],
        counts: PageCounts,
        item_rows: &[(i64, Vec<u8>)],
        fetched_at: i64,
        ttl: Duration,
    ) -> CacheResult<CacheMetadata> {
        let metadata = metadata_for_write(fetched_at, ttl);
        let item_count = sqlite_count(counts.covered);
        let available_count = sqlite_count(counts.available);
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        upsert_item_rows(&transaction, item_rows, metadata)?;
        transaction.execute(
            "INSERT INTO feeds (feed, payload, fetched_at, expires_at, item_count, available_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(feed) DO UPDATE SET
                 payload = excluded.payload,
                 fetched_at = excluded.fetched_at,
                 expires_at = excluded.expires_at,
                 item_count = excluded.item_count,
                 available_count = excluded.available_count
             WHERE (excluded.fetched_at > feeds.fetched_at
                       AND (excluded.item_count < feeds.item_count
                            OR (excluded.item_count >= feeds.item_count
                                AND excluded.available_count >= feeds.available_count)))
                OR (excluded.fetched_at = feeds.fetched_at
                       AND ((excluded.item_count > feeds.item_count
                             AND excluded.available_count >= feeds.available_count)
                            OR (excluded.item_count = feeds.item_count
                                AND excluded.available_count >= feeds.available_count)))",
            params![
                feed.as_str(),
                payload,
                metadata.fetched_at,
                metadata.expires_at,
                item_count,
                available_count,
            ],
        )?;
        let persisted = transaction.query_row(
            "SELECT fetched_at, expires_at, item_count FROM feeds WHERE feed = ?1",
            [feed.as_str()],
            metadata_from_counted_row,
        )?;
        transaction.commit()?;
        Ok(persisted)
    }

    /// Generic feed retrieval corresponding to [`Self::put_feed_value`].
    pub fn get_feed_value<T: DeserializeOwned>(
        &self,
        feed: Feed,
    ) -> CacheResult<Option<CacheEntry<T>>> {
        self.read_entry(
            "SELECT payload, fetched_at, expires_at, item_count FROM feeds WHERE feed = ?1",
            params![feed.as_str()],
        )
    }

    pub fn remove_feed(&self, feed: Feed) -> CacheResult<bool> {
        Ok(self
            .lock()?
            .execute("DELETE FROM feeds WHERE feed = ?1", [feed.as_str()])?
            > 0)
    }

    pub fn put_item(&self, item: &Item, ttl: Duration) -> CacheResult<CacheMetadata> {
        self.put_item_value(item.id, item, current_timestamp(), ttl)
    }

    pub fn get_item(&self, id: u64) -> CacheResult<Option<CacheEntry<Item>>> {
        self.get_item_value(id)
    }

    pub fn get_fresh_item(&self, id: u64) -> CacheResult<Option<Item>> {
        Ok(self
            .get_item(id)?
            .filter(|entry| entry.metadata.is_fresh())
            .map(CacheEntry::into_inner))
    }

    pub fn put_item_value<T: Serialize>(
        &self,
        id: u64,
        value: &T,
        fetched_at: i64,
        ttl: Duration,
    ) -> CacheResult<CacheMetadata> {
        let id = sqlite_id(id)?;
        let payload = encode(value)?;
        let metadata = metadata_for_write(fetched_at, ttl);
        self.lock()?.execute(
            "INSERT INTO items (id, payload, fetched_at, expires_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                 payload = excluded.payload,
                 fetched_at = excluded.fetched_at,
                 expires_at = excluded.expires_at",
            params![id, payload, metadata.fetched_at, metadata.expires_at],
        )?;
        Ok(metadata)
    }

    pub fn get_item_value<T: DeserializeOwned>(
        &self,
        id: u64,
    ) -> CacheResult<Option<CacheEntry<T>>> {
        self.read_entry(
            "SELECT payload, fetched_at, expires_at, NULL FROM items WHERE id = ?1",
            params![sqlite_id(id)?],
        )
    }

    pub fn remove_item(&self, id: u64) -> CacheResult<bool> {
        Ok(self
            .lock()?
            .execute("DELETE FROM items WHERE id = ?1", [sqlite_id(id)?])?
            > 0)
    }

    pub fn put_thread(&self, thread: &Thread, ttl: Duration) -> CacheResult<CacheMetadata> {
        let fetched_at = valid_or_current_timestamp(thread.fetched_at);
        let id = sqlite_id(thread.item.id)?;
        let payload = encode(thread)?;
        let item_rows = encode_item_rows(std::slice::from_ref(&thread.item))?;
        let metadata = metadata_for_write(fetched_at, ttl);
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        upsert_item_rows(&transaction, &item_rows, metadata)?;
        transaction.execute(
            "INSERT INTO threads (item_id, payload, fetched_at, expires_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(item_id) DO UPDATE SET
                 payload = excluded.payload,
                 fetched_at = excluded.fetched_at,
                 expires_at = excluded.expires_at",
            params![id, payload, metadata.fetched_at, metadata.expires_at],
        )?;
        transaction.commit()?;
        Ok(metadata)
    }

    pub fn get_thread(&self, id: u64) -> CacheResult<Option<CacheEntry<Thread>>> {
        let mut entry = self.get_thread_value::<Thread>(id)?;
        if let Some(cached) = &mut entry {
            cached.value.source = Source::Cache;
            cached.value.stale = cached.metadata.stale;
            cached.value.fetched_at = cached.metadata.fetched_at;
        }
        Ok(entry)
    }

    pub fn get_fresh_thread(&self, id: u64) -> CacheResult<Option<Thread>> {
        Ok(self
            .get_thread(id)?
            .filter(|entry| entry.metadata.is_fresh())
            .map(CacheEntry::into_inner))
    }

    pub fn put_thread_value<T: Serialize>(
        &self,
        id: u64,
        value: &T,
        fetched_at: i64,
        ttl: Duration,
    ) -> CacheResult<CacheMetadata> {
        let id = sqlite_id(id)?;
        let payload = encode(value)?;
        let metadata = metadata_for_write(fetched_at, ttl);
        self.lock()?.execute(
            "INSERT INTO threads (item_id, payload, fetched_at, expires_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(item_id) DO UPDATE SET
                 payload = excluded.payload,
                 fetched_at = excluded.fetched_at,
                 expires_at = excluded.expires_at",
            params![id, payload, metadata.fetched_at, metadata.expires_at],
        )?;
        Ok(metadata)
    }

    pub fn get_thread_value<T: DeserializeOwned>(
        &self,
        id: u64,
    ) -> CacheResult<Option<CacheEntry<T>>> {
        self.read_entry(
            "SELECT payload, fetched_at, expires_at, NULL FROM threads WHERE item_id = ?1",
            params![sqlite_id(id)?],
        )
    }

    pub fn remove_thread(&self, id: u64) -> CacheResult<bool> {
        Ok(self
            .lock()?
            .execute("DELETE FROM threads WHERE item_id = ?1", [sqlite_id(id)?])?
            > 0)
    }

    pub fn put_search(
        &self,
        query: &str,
        page: &StoryPage,
        ttl: Duration,
    ) -> CacheResult<CacheMetadata> {
        let fetched_at = valid_or_current_timestamp(page.fetched_at);
        let query = validate_key(query, MAX_SEARCH_KEY_BYTES, "search query")?;
        let payload = encode(page)?;
        let item_rows = encode_item_rows(&page.items)?;
        self.put_search_encoded(
            query,
            &payload,
            PageCounts {
                covered: page.covered_slots(),
                available: page.items.len(),
            },
            &item_rows,
            fetched_at,
            ttl,
        )
    }

    pub fn get_search(&self, query: &str) -> CacheResult<Option<CacheEntry<StoryPage>>> {
        let mut entry = self.get_search_value::<StoryPage>(query)?;
        if let Some(cached) = &mut entry {
            cached.value.source = Source::Cache;
            cached.value.stale = cached.metadata.stale;
            cached.value.fetched_at = cached.metadata.fetched_at;
        }
        Ok(entry)
    }

    pub fn get_fresh_search(&self, query: &str) -> CacheResult<Option<StoryPage>> {
        Ok(self
            .get_search(query)?
            .filter(|entry| entry.metadata.is_fresh())
            .map(CacheEntry::into_inner))
    }

    /// Reads a search page only when it contains at least `requested` items.
    pub fn get_search_for_limit(
        &self,
        query: &str,
        requested: usize,
    ) -> CacheResult<Option<CacheEntry<StoryPage>>> {
        Ok(self
            .get_search(query)?
            .filter(|entry| entry.contains_at_least(requested)))
    }

    /// Reads a search page only when it is fresh and contains at least `requested` items.
    pub fn get_fresh_search_for_limit(
        &self,
        query: &str,
        requested: usize,
    ) -> CacheResult<Option<StoryPage>> {
        Ok(self
            .get_search_for_limit(query, requested)?
            .filter(|entry| entry.metadata.is_fresh())
            .map(CacheEntry::into_inner))
    }

    pub fn put_search_value<T: Serialize>(
        &self,
        query: &str,
        value: &T,
        fetched_at: i64,
        ttl: Duration,
    ) -> CacheResult<CacheMetadata> {
        let query = validate_key(query, MAX_SEARCH_KEY_BYTES, "search query")?;
        let payload = encode(value)?;
        let counts = serialized_story_counts(&payload).unwrap_or(PageCounts {
            covered: 0,
            available: 0,
        });
        self.put_search_encoded(query, &payload, counts, &[], fetched_at, ttl)
    }

    fn put_search_encoded(
        &self,
        query: &str,
        payload: &[u8],
        counts: PageCounts,
        item_rows: &[(i64, Vec<u8>)],
        fetched_at: i64,
        ttl: Duration,
    ) -> CacheResult<CacheMetadata> {
        let metadata = metadata_for_write(fetched_at, ttl);
        let item_count = sqlite_count(counts.covered);
        let available_count = sqlite_count(counts.available);
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        upsert_item_rows(&transaction, item_rows, metadata)?;
        transaction.execute(
            "INSERT INTO searches (query, payload, fetched_at, expires_at, item_count, available_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(query) DO UPDATE SET
                 payload = excluded.payload,
                 fetched_at = excluded.fetched_at,
                 expires_at = excluded.expires_at,
                 item_count = excluded.item_count,
                 available_count = excluded.available_count
             WHERE (excluded.fetched_at > searches.fetched_at
                       AND (excluded.item_count < searches.item_count
                            OR (excluded.item_count >= searches.item_count
                                AND excluded.available_count >= searches.available_count)))
                OR (excluded.fetched_at = searches.fetched_at
                       AND ((excluded.item_count > searches.item_count
                             AND excluded.available_count >= searches.available_count)
                            OR (excluded.item_count = searches.item_count
                                AND excluded.available_count >= searches.available_count)))",
            params![
                query,
                payload,
                metadata.fetched_at,
                metadata.expires_at,
                item_count,
                available_count,
            ],
        )?;
        let persisted = transaction.query_row(
            "SELECT fetched_at, expires_at, item_count FROM searches WHERE query = ?1",
            [query],
            metadata_from_counted_row,
        )?;
        transaction.commit()?;
        Ok(persisted)
    }

    pub fn get_search_value<T: DeserializeOwned>(
        &self,
        query: &str,
    ) -> CacheResult<Option<CacheEntry<T>>> {
        let query = validate_key(query, MAX_SEARCH_KEY_BYTES, "search query")?;
        self.read_entry(
            "SELECT payload, fetched_at, expires_at, item_count FROM searches WHERE query = ?1",
            [query],
        )
    }

    pub fn remove_search(&self, query: &str) -> CacheResult<bool> {
        let query = validate_key(query, MAX_SEARCH_KEY_BYTES, "search query")?;
        Ok(self
            .lock()?
            .execute("DELETE FROM searches WHERE query = ?1", [query])?
            > 0)
    }

    /// Adds or updates a persistent bookmark. Bookmarks are never TTL-pruned.
    pub fn add_bookmark(&self, item: &Item) -> CacheResult<()> {
        self.add_bookmark_at(item, current_timestamp())
    }

    /// Alias for [`Self::add_bookmark`] used by command-oriented callers.
    pub fn set_bookmark(&self, item: &Item) -> CacheResult<()> {
        self.add_bookmark(item)
    }

    pub fn add_bookmark_at(&self, item: &Item, bookmarked_at: i64) -> CacheResult<()> {
        let mut canonical = item.clone();
        canonical.rank = None;
        let payload = encode(&canonical)?;
        self.lock()?.execute(
            "INSERT INTO bookmarks (item_id, payload, bookmarked_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(item_id) DO UPDATE SET payload = excluded.payload",
            params![sqlite_id(item.id)?, payload, bookmarked_at],
        )?;
        Ok(())
    }

    pub fn remove_bookmark(&self, id: u64) -> CacheResult<bool> {
        Ok(self
            .lock()?
            .execute("DELETE FROM bookmarks WHERE item_id = ?1", [sqlite_id(id)?])?
            > 0)
    }

    pub fn is_bookmarked(&self, id: u64) -> CacheResult<bool> {
        let exists = self.lock()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM bookmarks WHERE item_id = ?1)",
            [sqlite_id(id)?],
            |row| row.get(0),
        )?;
        Ok(exists)
    }

    pub fn bookmarks(&self) -> CacheResult<Vec<Item>> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare("SELECT payload FROM bookmarks ORDER BY bookmarked_at DESC, item_id DESC")?;
        let rows = statement.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
        let payloads = rows.collect::<Result<Vec<_>, _>>()?;
        payloads
            .iter()
            .map(|payload| serde_json::from_slice(payload).map_err(CacheError::from))
            .collect()
    }

    /// Marks one story as read. Per-item rows make concurrent UI sessions
    /// independent instead of replacing one shared serialized set.
    pub fn set_read(&self, id: u64) -> CacheResult<()> {
        self.lock()?.execute(
            "INSERT INTO read_items (item_id, read_at) VALUES (?1, ?2)
             ON CONFLICT(item_id) DO UPDATE SET read_at = excluded.read_at",
            params![sqlite_id(id)?, current_timestamp()],
        )?;
        Ok(())
    }

    pub fn remove_read(&self, id: u64) -> CacheResult<bool> {
        Ok(self.lock()?.execute(
            "DELETE FROM read_items WHERE item_id = ?1",
            [sqlite_id(id)?],
        )? > 0)
    }

    pub fn read_items(&self) -> CacheResult<Vec<u64>> {
        let connection = self.lock()?;
        let mut statement =
            connection.prepare("SELECT item_id FROM read_items ORDER BY read_at, item_id")?;
        let rows = statement.query_map([], |row| row.get::<_, i64>(0))?;
        rows.map(|row| {
            let id = row?;
            u64::try_from(id).map_err(|_| CacheError::InvalidItemId(u64::MAX))
        })
        .collect()
    }

    /// Writes an application setting as an opaque UTF-8 value.
    pub fn set_setting(&self, key: &str, value: &str) -> CacheResult<()> {
        let key = validate_key(key, MAX_SETTING_KEY_BYTES, "setting key")?;
        if value.len() > MAX_SETTING_VALUE_BYTES {
            return Err(CacheError::ValueTooLarge {
                actual: value.len(),
                limit: MAX_SETTING_VALUE_BYTES,
            });
        }
        self.lock()?.execute(
            "INSERT INTO settings (key, value, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET
                 value = excluded.value,
                 updated_at = excluded.updated_at",
            params![key, value, current_timestamp()],
        )?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> CacheResult<Option<String>> {
        let key = validate_key(key, MAX_SETTING_KEY_BYTES, "setting key")?;
        Ok(self
            .lock()?
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    pub fn remove_setting(&self, key: &str) -> CacheResult<bool> {
        let key = validate_key(key, MAX_SETTING_KEY_BYTES, "setting key")?;
        Ok(self
            .lock()?
            .execute("DELETE FROM settings WHERE key = ?1", [key])?
            > 0)
    }

    pub fn settings(&self) -> CacheResult<BTreeMap<String, String>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare("SELECT key, value FROM settings ORDER BY key")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn set_json_setting<T: Serialize>(&self, key: &str, value: &T) -> CacheResult<()> {
        self.set_setting(key, &serde_json::to_string(value)?)
    }

    pub fn get_json_setting<T: DeserializeOwned>(&self, key: &str) -> CacheResult<Option<T>> {
        self.get_setting(key)?
            .map(|value| serde_json::from_str(&value).map_err(CacheError::from))
            .transpose()
    }

    /// Returns row counts and approximate storage sizes.
    pub fn stats(&self) -> CacheResult<CacheStats> {
        let connection = self.lock()?;
        let now = current_timestamp();
        let mut stats = CacheStats {
            feeds: count_rows(&connection, "feeds")?,
            items: count_rows(&connection, "items")?,
            threads: count_rows(&connection, "threads")?,
            searches: count_rows(&connection, "searches")?,
            bookmarks: count_rows(&connection, "bookmarks")?,
            read_items: count_rows(&connection, "read_items")?,
            settings: count_rows(&connection, "settings")?,
            stale_entries: 0,
            payload_bytes: 0,
            database_bytes: 0,
        };

        stats.stale_entries = row_u64(
            &connection,
            "SELECT
                (SELECT COUNT(*) FROM feeds WHERE expires_at <= ?1) +
                (SELECT COUNT(*) FROM items WHERE expires_at <= ?1) +
                (SELECT COUNT(*) FROM threads WHERE expires_at <= ?1) +
                (SELECT COUNT(*) FROM searches WHERE expires_at <= ?1)",
            [now],
        )?;
        stats.payload_bytes = row_u64(
            &connection,
            "SELECT
                (SELECT COALESCE(SUM(length(payload)), 0) FROM feeds) +
                (SELECT COALESCE(SUM(length(payload)), 0) FROM items) +
                (SELECT COALESCE(SUM(length(payload)), 0) FROM threads) +
                (SELECT COALESCE(SUM(length(payload)), 0) FROM searches) +
                (SELECT COALESCE(SUM(length(payload)), 0) FROM bookmarks)",
            [],
        )?;
        let page_count = row_u64(&connection, "PRAGMA page_count", [])?;
        let page_size = row_u64(&connection, "PRAGMA page_size", [])?;
        stats.database_bytes = page_count.saturating_mul(page_size);
        Ok(stats)
    }

    /// Deletes expired feed, item, thread, and search rows.
    ///
    /// Bookmarks, read state, and settings are deliberately not affected.
    pub fn prune(&self) -> CacheResult<PruneStats> {
        self.prune_expired_at(current_timestamp())
    }

    /// Deterministic variant of [`Self::prune`] for tests and maintenance.
    pub fn prune_expired_at(&self, timestamp: i64) -> CacheResult<PruneStats> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let stats = PruneStats {
            feeds: transaction.execute("DELETE FROM feeds WHERE expires_at <= ?1", [timestamp])?,
            items: transaction.execute("DELETE FROM items WHERE expires_at <= ?1", [timestamp])?,
            threads: transaction
                .execute("DELETE FROM threads WHERE expires_at <= ?1", [timestamp])?,
            searches: transaction
                .execute("DELETE FROM searches WHERE expires_at <= ?1", [timestamp])?,
        };
        transaction.commit()?;
        Ok(stats)
    }

    /// Clears only TTL-managed cached network data.
    pub fn clear(&self) -> CacheResult<PruneStats> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let stats = clear_cache_tables(&transaction)?;
        transaction.commit()?;
        Ok(stats)
    }

    /// Clears cached network data, bookmarks, read state, and settings.
    ///
    /// This intentionally destructive variant is named separately so the
    /// ordinary `clear` operation cannot erase user-owned state.
    pub fn clear_all(&self) -> CacheResult<usize> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let cache_rows = clear_cache_tables(&transaction)?.total();
        let bookmarks = transaction.execute("DELETE FROM bookmarks", [])?;
        let read_items = transaction.execute("DELETE FROM read_items", [])?;
        let settings = transaction.execute("DELETE FROM settings", [])?;
        transaction.commit()?;
        Ok(cache_rows + bookmarks + read_items + settings)
    }

    fn read_entry<T: DeserializeOwned, P: rusqlite::Params>(
        &self,
        sql: &str,
        parameters: P,
    ) -> CacheResult<Option<CacheEntry<T>>> {
        let row = self
            .lock()?
            .query_row(sql, parameters, |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            })
            .optional()?;

        row.map(|(payload, fetched_at, expires_at, item_count)| {
            let value = serde_json::from_slice(&payload)?;
            Ok(CacheEntry {
                value,
                metadata: metadata_from_values(fetched_at, expires_at, item_count),
            })
        })
        .transpose()
    }

    fn lock(&self) -> CacheResult<MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| CacheError::LockPoisoned)
    }
}

#[allow(clippy::too_many_lines)]
fn migrate_connection(connection: &mut Connection) -> CacheResult<()> {
    // Acquire the database write lock before reading the version. This makes
    // first-open migration safe when multiple processes start concurrently.
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version = transaction.query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?;
    if version > SCHEMA_VERSION {
        return Err(CacheError::UnsupportedSchema {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }

    if version < 1 {
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                 version INTEGER PRIMARY KEY,
                 applied_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS feeds (
                 feed TEXT PRIMARY KEY NOT NULL,
                 payload BLOB NOT NULL CHECK(length(payload) > 0),
                 fetched_at INTEGER NOT NULL,
                 expires_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS items (
                 id INTEGER PRIMARY KEY NOT NULL CHECK(id >= 0),
                 payload BLOB NOT NULL CHECK(length(payload) > 0),
                 fetched_at INTEGER NOT NULL,
                 expires_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS threads (
                 item_id INTEGER PRIMARY KEY NOT NULL CHECK(item_id >= 0),
                 payload BLOB NOT NULL CHECK(length(payload) > 0),
                 fetched_at INTEGER NOT NULL,
                 expires_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS searches (
                 query TEXT PRIMARY KEY NOT NULL,
                 payload BLOB NOT NULL CHECK(length(payload) > 0),
                 fetched_at INTEGER NOT NULL,
                 expires_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS bookmarks (
                 item_id INTEGER PRIMARY KEY NOT NULL CHECK(item_id >= 0),
                 payload BLOB NOT NULL CHECK(length(payload) > 0),
                 bookmarked_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS settings (
                 key TEXT PRIMARY KEY NOT NULL,
                 value TEXT NOT NULL,
                 updated_at INTEGER NOT NULL
             );",
        )?;
        record_migration(&transaction, 1)?;
        transaction.execute_batch("PRAGMA user_version = 1;")?;
    }

    if version < 2 {
        // Some prerelease v1 databases did not record migrations separately.
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                 version INTEGER PRIMARY KEY,
                 applied_at INTEGER NOT NULL
             );",
        )?;
        transaction.execute_batch(
            "CREATE INDEX IF NOT EXISTS feeds_expiry_idx ON feeds(expires_at);
             CREATE INDEX IF NOT EXISTS items_expiry_idx ON items(expires_at);
             CREATE INDEX IF NOT EXISTS threads_expiry_idx ON threads(expires_at);
             CREATE INDEX IF NOT EXISTS searches_expiry_idx ON searches(expires_at);
             CREATE INDEX IF NOT EXISTS bookmarks_order_idx
                 ON bookmarks(bookmarked_at DESC, item_id DESC);",
        )?;
        record_migration(&transaction, 2)?;
        transaction.execute_batch("PRAGMA user_version = 2;")?;
    }

    if version < 3 {
        transaction.execute_batch(
            "ALTER TABLE feeds
                 ADD COLUMN item_count INTEGER NOT NULL DEFAULT 0 CHECK(item_count >= 0);
             ALTER TABLE searches
                 ADD COLUMN item_count INTEGER NOT NULL DEFAULT 0 CHECK(item_count >= 0);",
        )?;
        backfill_page_counts(&transaction)?;
        record_migration(&transaction, 3)?;
        transaction.execute_batch("PRAGMA user_version = 3;")?;
    }

    if version < 4 {
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (
                 key TEXT PRIMARY KEY NOT NULL,
                 value TEXT NOT NULL,
                 updated_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS read_items (
                 item_id INTEGER PRIMARY KEY NOT NULL CHECK(item_id >= 0),
                 read_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS read_items_order_idx
                 ON read_items(read_at, item_id);",
        )?;
        migrate_legacy_read_setting(&transaction)?;
        record_migration(&transaction, 4)?;
        transaction.execute_batch("PRAGMA user_version = 4;")?;
    }

    if version < 5 {
        transaction.execute_batch(
            "ALTER TABLE feeds
                 ADD COLUMN available_count INTEGER NOT NULL DEFAULT 0 CHECK(available_count >= 0);
             ALTER TABLE searches
                 ADD COLUMN available_count INTEGER NOT NULL DEFAULT 0 CHECK(available_count >= 0);",
        )?;
        backfill_available_counts(&transaction)?;
        // Early builds of this branch created schema v4 before the legacy
        // read-setting import was added. Re-running the idempotent import here
        // repairs those databases while v5 is still the shipping migration.
        migrate_legacy_read_setting(&transaction)?;
        record_migration(&transaction, 5)?;
        transaction.execute_batch("PRAGMA user_version = 5;")?;
    }

    transaction.commit()?;
    Ok(())
}

fn migrate_legacy_read_setting(transaction: &Transaction<'_>) -> CacheResult<()> {
    let legacy = transaction
        .query_row(
            "SELECT value FROM settings WHERE key = 'read.v1'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(legacy) = legacy else {
        return Ok(());
    };
    let Ok(ids) = serde_json::from_str::<BTreeSet<u64>>(&legacy) else {
        // Preserve corrupt legacy data for manual recovery instead of making
        // cache migration prevent the application from starting.
        return Ok(());
    };
    let read_at = current_timestamp();
    for id in ids {
        transaction.execute(
            "INSERT OR IGNORE INTO read_items (item_id, read_at) VALUES (?1, ?2)",
            params![sqlite_id(id)?, read_at],
        )?;
    }
    transaction.execute("DELETE FROM settings WHERE key = 'read.v1'", [])?;
    Ok(())
}

fn backfill_page_counts(transaction: &Transaction<'_>) -> CacheResult<()> {
    let feed_rows = {
        let mut statement = transaction.prepare("SELECT feed, payload FROM feeds")?;
        let rows = statement.query_map([], |row| {
            let payload = row.get::<_, Vec<u8>>(1)?;
            Ok((
                row.get::<_, String>(0)?,
                serialized_story_counts(&payload).map(|counts| counts.covered),
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for (feed, item_count) in feed_rows {
        if let Some(item_count) = item_count {
            transaction.execute(
                "UPDATE feeds SET item_count = ?1 WHERE feed = ?2",
                params![sqlite_count(item_count), feed],
            )?;
        }
    }

    let search_rows = {
        let mut statement = transaction.prepare("SELECT query, payload FROM searches")?;
        let rows = statement.query_map([], |row| {
            let payload = row.get::<_, Vec<u8>>(1)?;
            Ok((
                row.get::<_, String>(0)?,
                serialized_story_counts(&payload).map(|counts| counts.covered),
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for (query, item_count) in search_rows {
        if let Some(item_count) = item_count {
            transaction.execute(
                "UPDATE searches SET item_count = ?1 WHERE query = ?2",
                params![sqlite_count(item_count), query],
            )?;
        }
    }
    Ok(())
}

fn backfill_available_counts(transaction: &Transaction<'_>) -> CacheResult<()> {
    for (table, key) in [("feeds", "feed"), ("searches", "query")] {
        let select = format!("SELECT {key}, payload FROM {table}");
        let rows = {
            let mut statement = transaction.prepare(&select)?;
            let rows = statement.query_map([], |row| {
                let payload = row.get::<_, Vec<u8>>(1)?;
                Ok((
                    row.get::<_, String>(0)?,
                    serialized_story_counts(&payload).map(|counts| counts.available),
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let update = format!("UPDATE {table} SET available_count = ?1 WHERE {key} = ?2");
        for (row_key, available_count) in rows {
            if let Some(available_count) = available_count {
                transaction.execute(&update, params![sqlite_count(available_count), row_key])?;
            }
        }
    }
    Ok(())
}

fn record_migration(transaction: &Transaction<'_>, version: u32) -> CacheResult<()> {
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
        params![version, current_timestamp()],
    )?;
    Ok(())
}

fn clear_cache_tables(transaction: &Transaction<'_>) -> CacheResult<PruneStats> {
    Ok(PruneStats {
        feeds: transaction.execute("DELETE FROM feeds", [])?,
        items: transaction.execute("DELETE FROM items", [])?,
        threads: transaction.execute("DELETE FROM threads", [])?,
        searches: transaction.execute("DELETE FROM searches", [])?,
    })
}

fn count_rows(connection: &Connection, table: &str) -> CacheResult<u64> {
    // `table` is selected only from fixed literals in `stats`; no user input is
    // interpolated into SQL.
    let sql = match table {
        "feeds" => "SELECT COUNT(*) FROM feeds",
        "items" => "SELECT COUNT(*) FROM items",
        "threads" => "SELECT COUNT(*) FROM threads",
        "searches" => "SELECT COUNT(*) FROM searches",
        "bookmarks" => "SELECT COUNT(*) FROM bookmarks",
        "read_items" => "SELECT COUNT(*) FROM read_items",
        "settings" => "SELECT COUNT(*) FROM settings",
        _ => return Err(CacheError::InvalidKey("unknown cache table")),
    };
    row_u64(connection, sql, [])
}

fn row_u64<P: rusqlite::Params>(
    connection: &Connection,
    sql: &str,
    parameters: P,
) -> CacheResult<u64> {
    let value = connection.query_row(sql, parameters, |row| row.get::<_, i64>(0))?;
    Ok(u64::try_from(value).unwrap_or(0))
}

fn metadata_from_counted_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CacheMetadata> {
    Ok(metadata_from_values(
        row.get(0)?,
        row.get(1)?,
        Some(row.get(2)?),
    ))
}

fn metadata_from_values(
    fetched_at: i64,
    expires_at: i64,
    item_count: Option<i64>,
) -> CacheMetadata {
    CacheMetadata {
        fetched_at,
        expires_at,
        stale: expires_at <= current_timestamp(),
        item_count: item_count.and_then(|count| usize::try_from(count).ok()),
    }
}

fn serialized_story_counts(payload: &[u8]) -> Option<PageCounts> {
    serde_json::from_slice::<StoryPage>(payload)
        .ok()
        .map(|page| PageCounts {
            covered: page.covered_slots(),
            available: page.items.len(),
        })
}

fn sqlite_count(count: usize) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

fn encode_item_rows(items: &[Item]) -> CacheResult<Vec<(i64, Vec<u8>)>> {
    items
        .iter()
        .map(|item| {
            let mut canonical = item.clone();
            canonical.rank = None;
            Ok((sqlite_id(item.id)?, encode(&canonical)?))
        })
        .collect()
}

fn upsert_item_rows(
    transaction: &Transaction<'_>,
    item_rows: &[(i64, Vec<u8>)],
    metadata: CacheMetadata,
) -> CacheResult<()> {
    let mut statement = transaction.prepare(
        "INSERT INTO items (id, payload, fetched_at, expires_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET
             payload = excluded.payload,
             fetched_at = excluded.fetched_at,
             expires_at = excluded.expires_at
         WHERE excluded.fetched_at >= items.fetched_at",
    )?;
    for (id, payload) in item_rows {
        statement.execute(params![
            id,
            payload,
            metadata.fetched_at,
            metadata.expires_at
        ])?;
    }
    Ok(())
}

fn encode<T: Serialize>(value: &T) -> CacheResult<Vec<u8>> {
    let payload = serde_json::to_vec(value)?;
    if payload.len() > MAX_CACHE_VALUE_BYTES {
        return Err(CacheError::ValueTooLarge {
            actual: payload.len(),
            limit: MAX_CACHE_VALUE_BYTES,
        });
    }
    Ok(payload)
}

fn validate_key<'a>(value: &'a str, maximum: usize, label: &'static str) -> CacheResult<&'a str> {
    let value = value.trim();
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(CacheError::InvalidKey(label));
    }
    Ok(value)
}

fn sqlite_id(id: u64) -> CacheResult<i64> {
    i64::try_from(id).map_err(|_| CacheError::InvalidItemId(id))
}

fn metadata_for_write(fetched_at: i64, ttl: Duration) -> CacheMetadata {
    let fetched_at = valid_or_current_timestamp(fetched_at);
    let seconds = ttl
        .as_secs()
        .saturating_add(u64::from(ttl.subsec_nanos() > 0));
    let seconds = i64::try_from(seconds).unwrap_or(i64::MAX);
    let expires_at = fetched_at.saturating_add(seconds);
    CacheMetadata {
        fetched_at,
        expires_at,
        stale: expires_at <= current_timestamp(),
        item_count: None,
    }
}

fn valid_or_current_timestamp(timestamp: i64) -> i64 {
    if timestamp > 0 {
        timestamp
    } else {
        current_timestamp()
    }
}

fn current_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
#[allow(clippy::duration_suboptimal_units)] // `from_mins` is newer than the crate's MSRV.
mod tests {
    use super::*;

    fn item(id: u64) -> Item {
        Item {
            id,
            title: Some(format!("story {id}")),
            item_type: "story".to_owned(),
            ..Item::default()
        }
    }

    fn page(feed: Feed, count: usize, fetched_at: i64) -> StoryPage {
        StoryPage {
            feed,
            query: None,
            items: (1..=u64::try_from(count).expect("test count fits u64"))
                .map(item)
                .collect(),
            slot_count: count,
            source: Source::Firebase,
            stale: false,
            fetched_at,
        }
    }

    #[test]
    fn one_item_page_is_upgraded_before_it_satisfies_limit_thirty() {
        let cache = Cache::open_in_memory().expect("cache opens");
        let now = current_timestamp();
        cache
            .put_feed(&page(Feed::New, 1, now), Duration::from_secs(60))
            .expect("small page stores");

        let small = cache
            .get_feed_for_limit(Feed::New, 1)
            .expect("small page reads")
            .expect("one item is sufficient");
        assert_eq!(small.metadata.item_count, Some(1));
        assert!(
            cache
                .get_feed_for_limit(Feed::New, 30)
                .expect("limit checks")
                .is_none()
        );

        cache
            .put_feed(&page(Feed::New, 30, now + 1), Duration::from_secs(60))
            .expect("large page stores");
        let upgraded = cache
            .get_feed_for_limit(Feed::New, 30)
            .expect("large page reads")
            .expect("thirty items are sufficient");
        assert_eq!(upgraded.value.items.len(), 30);
        assert_eq!(upgraded.metadata.item_count, Some(30));
    }

    #[test]
    fn ranked_prefix_coverage_survives_an_unreadable_final_slot() {
        let cache = Cache::open_in_memory().expect("cache opens");
        let mut ranked = page(Feed::Top, 59, current_timestamp());
        ranked.slot_count = 60;
        for (index, item) in ranked.items.iter_mut().enumerate() {
            item.rank = Some(index + 1);
        }

        cache
            .put_feed(&ranked, Duration::from_secs(60))
            .expect("ranked page stores");
        let cached = cache
            .get_feed_for_limit(Feed::Top, 60)
            .expect("ranked page reads")
            .expect("sixty upstream slots are covered");
        assert_eq!(cached.metadata.item_count, Some(60));
        assert_eq!(cached.value.items.len(), 59);
    }

    #[test]
    fn newer_smaller_feed_and_search_puts_replace_stale_larger_pages() {
        let cache = Cache::open_in_memory().expect("cache opens");
        let now = current_timestamp();
        let large = page(Feed::Top, 30, now);
        let mut small = page(Feed::Top, 1, now + 1);
        small.query = Some("rust".to_owned());

        cache
            .put_feed(&large, Duration::from_secs(60))
            .expect("large feed stores");
        let replaced_feed_metadata = cache
            .put_feed(&small, Duration::from_secs(120))
            .expect("newer smaller feed replaces");
        assert!(
            cache
                .get_feed_for_limit(Feed::Top, 30)
                .expect("feed reads")
                .is_none()
        );
        let replaced_feed = cache
            .get_feed_for_limit(Feed::Top, 1)
            .expect("feed reads")
            .expect("small feed remains");
        assert_eq!(replaced_feed.value.items.len(), 1);
        assert_eq!(replaced_feed_metadata.fetched_at, now + 1);
        assert_eq!(replaced_feed_metadata.item_count, Some(1));
        let still_newer = cache
            .put_feed(&large, Duration::from_secs(60))
            .expect("older completion is ignored");
        assert_eq!(still_newer.fetched_at, now + 1);
        assert_eq!(still_newer.item_count, Some(1));

        cache
            .put_search("rust", &large, Duration::from_secs(60))
            .expect("large search stores");
        let replaced_search_metadata = cache
            .put_search("rust", &small, Duration::from_secs(120))
            .expect("newer smaller search replaces");
        assert!(
            cache
                .get_search_for_limit("rust", 30)
                .expect("search reads")
                .is_none()
        );
        let replaced_search = cache
            .get_search_for_limit("rust", 1)
            .expect("search reads")
            .expect("small search remains");
        assert_eq!(replaced_search.value.items.len(), 1);
        assert_eq!(replaced_search_metadata.fetched_at, now + 1);
        assert_eq!(replaced_search_metadata.item_count, Some(1));
        let still_newer = cache
            .put_search("rust", &large, Duration::from_secs(60))
            .expect("older search completion is ignored");
        assert_eq!(still_newer.fetched_at, now + 1);
        assert_eq!(still_newer.item_count, Some(1));
    }

    #[test]
    fn transient_partial_refresh_does_not_replace_equal_coverage() {
        let cache = Cache::open_in_memory().expect("cache opens");
        let now = current_timestamp();
        let complete = page(Feed::Top, 30, now);
        let mut partial = complete.clone();
        partial.items.pop();
        partial.fetched_at = now + 1;

        cache
            .put_feed(&complete, Duration::from_secs(60))
            .expect("complete feed stores");
        let retained = cache
            .put_feed(&partial, Duration::from_secs(60))
            .expect("partial feed is evaluated");
        assert_eq!(retained.fetched_at, now);
        assert_eq!(
            cache
                .get_feed_for_limit(Feed::Top, 30)
                .expect("feed reads")
                .expect("complete feed remains")
                .value
                .items
                .len(),
            30
        );

        cache
            .put_search("rust", &complete, Duration::from_secs(60))
            .expect("complete search stores");
        let retained = cache
            .put_search("rust", &partial, Duration::from_secs(60))
            .expect("partial search is evaluated");
        assert_eq!(retained.fetched_at, now);

        let mut sparse = page(Feed::Top, 1, now + 2);
        sparse.slot_count = 500;
        sparse.items[0].rank = Some(500);
        let retained = cache
            .put_feed(&sparse, Duration::from_secs(60))
            .expect("sparse larger prefix is evaluated");
        assert_eq!(retained.fetched_at, now);
        assert_eq!(retained.item_count, Some(30));
    }

    #[test]
    fn feed_and_search_puts_populate_the_item_cache() {
        let cache = Cache::open_in_memory().expect("cache opens");
        let now = current_timestamp();
        let mut ranked_feed = page(Feed::Show, 3, now);
        ranked_feed.items[1].rank = Some(2);
        cache
            .put_feed(&ranked_feed, Duration::from_secs(60))
            .expect("feed stores");
        let shared_item = cache
            .get_item(2)
            .expect("contained feed item reads")
            .expect("contained feed item exists")
            .value;
        assert_eq!(shared_item.id, 2);
        assert_eq!(shared_item.rank, None);

        let mut search_page = page(Feed::Top, 1, now);
        search_page.query = Some("cached".to_owned());
        search_page.items[0] = item(99);
        search_page.items[0].rank = Some(1);
        cache
            .put_search("cached", &search_page, Duration::from_secs(60))
            .expect("search stores");
        let shared_item = cache
            .get_item(99)
            .expect("contained search item reads")
            .expect("contained search item exists")
            .value;
        assert_eq!(shared_item.id, 99);
        assert_eq!(shared_item.rank, None);
    }

    #[test]
    fn thread_put_populates_the_root_item_cache() {
        let cache = Cache::open_in_memory().expect("cache opens");
        let root = item(77);
        let thread = Thread {
            item: root.clone(),
            comments: Vec::new(),
            source: Source::Firebase,
            stale: false,
            fetched_at: current_timestamp(),
        };

        cache
            .put_thread(&thread, Duration::from_secs(60))
            .expect("thread stores");

        assert_eq!(
            cache
                .get_item(77)
                .expect("root item reads")
                .expect("root item exists")
                .value,
            root
        );
    }

    #[test]
    fn version_two_migration_backfills_page_counts() {
        let directory = tempfile::tempdir().expect("tempdir creates");
        let path = directory.path().join("v2.sqlite3");
        let connection = Connection::open(&path).expect("legacy database opens");
        connection
            .execute_batch(
                "CREATE TABLE schema_migrations (
                     version INTEGER PRIMARY KEY,
                     applied_at INTEGER NOT NULL
                 );
                 CREATE TABLE feeds (
                     feed TEXT PRIMARY KEY NOT NULL,
                     payload BLOB NOT NULL CHECK(length(payload) > 0),
                     fetched_at INTEGER NOT NULL,
                     expires_at INTEGER NOT NULL
                 );
                 CREATE TABLE searches (
                     query TEXT PRIMARY KEY NOT NULL,
                     payload BLOB NOT NULL CHECK(length(payload) > 0),
                     fetched_at INTEGER NOT NULL,
                     expires_at INTEGER NOT NULL
                 );
                 PRAGMA user_version = 2;",
            )
            .expect("legacy schema creates");
        let legacy_page = page(Feed::Best, 7, current_timestamp());
        let payload = serde_json::to_vec(&legacy_page).expect("page serializes");
        connection
            .execute(
                "INSERT INTO feeds (feed, payload, fetched_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![Feed::Best.as_str(), &payload, 10_i64, i64::MAX],
            )
            .expect("legacy feed inserts");
        connection
            .execute(
                "INSERT INTO searches (query, payload, fetched_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params!["rust", &payload, 10_i64, i64::MAX],
            )
            .expect("legacy search inserts");
        drop(connection);

        let cache = Cache::open(&path).expect("legacy database migrates");
        assert_eq!(cache.schema_version().expect("version reads"), 5);
        assert_eq!(
            cache
                .get_feed(Feed::Best)
                .expect("feed reads")
                .expect("feed exists")
                .metadata
                .item_count,
            Some(7)
        );
        assert_eq!(
            cache
                .get_search("rust")
                .expect("search reads")
                .expect("search exists")
                .metadata
                .item_count,
            Some(7)
        );
    }

    #[test]
    fn version_three_migration_imports_legacy_read_state() {
        let directory = tempfile::tempdir().expect("tempdir creates");
        let path = directory.path().join("v3.sqlite3");
        let connection = Connection::open(&path).expect("legacy database opens");
        connection
            .execute_batch(
                "CREATE TABLE schema_migrations (
                     version INTEGER PRIMARY KEY,
                     applied_at INTEGER NOT NULL
                 );
                 CREATE TABLE feeds (
                     feed TEXT PRIMARY KEY NOT NULL,
                     payload BLOB NOT NULL,
                     fetched_at INTEGER NOT NULL,
                     expires_at INTEGER NOT NULL,
                     item_count INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE searches (
                     query TEXT PRIMARY KEY NOT NULL,
                     payload BLOB NOT NULL,
                     fetched_at INTEGER NOT NULL,
                     expires_at INTEGER NOT NULL,
                     item_count INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE settings (
                     key TEXT PRIMARY KEY NOT NULL,
                     value TEXT NOT NULL,
                     updated_at INTEGER NOT NULL
                 );
                 INSERT INTO settings (key, value, updated_at)
                     VALUES ('read.v1', '[42,99]', 1);
                 PRAGMA user_version = 3;",
            )
            .expect("legacy schema creates");
        drop(connection);

        let cache = Cache::open(&path).expect("legacy database migrates");
        assert_eq!(cache.schema_version().expect("version reads"), 5);
        assert_eq!(
            cache.read_items().expect("read state imports"),
            vec![42, 99]
        );
        assert_eq!(
            cache.get_setting("read.v1").expect("legacy setting checks"),
            None
        );
    }

    #[test]
    fn version_four_migration_repairs_legacy_read_state() {
        let directory = tempfile::tempdir().expect("tempdir creates");
        let path = directory.path().join("v4.sqlite3");
        let connection = Connection::open(&path).expect("legacy database opens");
        connection
            .execute_batch(
                "CREATE TABLE schema_migrations (
                     version INTEGER PRIMARY KEY,
                     applied_at INTEGER NOT NULL
                 );
                 CREATE TABLE feeds (
                     feed TEXT PRIMARY KEY NOT NULL,
                     payload BLOB NOT NULL,
                     fetched_at INTEGER NOT NULL,
                     expires_at INTEGER NOT NULL,
                     item_count INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE searches (
                     query TEXT PRIMARY KEY NOT NULL,
                     payload BLOB NOT NULL,
                     fetched_at INTEGER NOT NULL,
                     expires_at INTEGER NOT NULL,
                     item_count INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE settings (
                     key TEXT PRIMARY KEY NOT NULL,
                     value TEXT NOT NULL,
                     updated_at INTEGER NOT NULL
                 );
                 CREATE TABLE read_items (
                     item_id INTEGER PRIMARY KEY NOT NULL,
                     read_at INTEGER NOT NULL
                 );
                 INSERT INTO settings (key, value, updated_at)
                     VALUES ('read.v1', '[42,99]', 1);
                 PRAGMA user_version = 4;",
            )
            .expect("legacy schema creates");
        drop(connection);

        let cache = Cache::open(&path).expect("legacy database migrates");
        assert_eq!(cache.schema_version().expect("version reads"), 5);
        assert_eq!(
            cache.read_items().expect("read state imports"),
            vec![42, 99]
        );
        assert_eq!(
            cache.get_setting("read.v1").expect("legacy setting checks"),
            None
        );
    }

    #[test]
    fn migrates_and_round_trips_all_persistent_kinds() {
        let cache = Cache::open_in_memory().expect("cache opens");
        assert_eq!(
            cache.schema_version().expect("version reads"),
            SCHEMA_VERSION
        );

        let story = item(42);
        cache
            .put_item(&story, Duration::from_secs(60))
            .expect("item stores");
        cache.add_bookmark(&story).expect("bookmark stores");
        cache.set_read(story.id).expect("read state stores");
        cache
            .set_setting("theme", "midnight")
            .expect("setting stores");

        assert_eq!(
            cache.get_item(42).expect("item reads").unwrap().value,
            story
        );
        assert!(cache.is_bookmarked(42).expect("bookmark checks"));
        assert_eq!(cache.read_items().expect("read state reads"), vec![42]);
        assert_eq!(cache.bookmarks().expect("bookmarks read"), vec![story]);
        assert_eq!(
            cache
                .get_setting("theme")
                .expect("setting reads")
                .as_deref(),
            Some("midnight")
        );
    }

    #[test]
    fn stale_rows_remain_readable_until_pruned() {
        let cache = Cache::open_in_memory().expect("cache opens");
        cache
            .put_item_value(7, &item(7), 1, Duration::ZERO)
            .expect("item stores");

        let cached = cache.get_item(7).expect("item reads").unwrap();
        assert!(cached.is_stale());
        assert!(cache.get_fresh_item(7).expect("fresh read").is_none());
        assert_eq!(cache.stats().expect("stats read").stale_entries, 1);

        let pruned = cache.prune_expired_at(i64::MAX).expect("cache prunes");
        assert_eq!(pruned.items, 1);
        assert!(cache.get_item(7).expect("item reads").is_none());
    }

    #[test]
    fn clear_preserves_user_owned_rows() {
        let cache = Cache::open_in_memory().expect("cache opens");
        let story = item(9);
        cache
            .put_item(&story, Duration::from_secs(60))
            .expect("item stores");
        cache.add_bookmark(&story).expect("bookmark stores");
        cache.set_read(story.id).expect("read state stores");
        cache
            .set_setting("theme", "classic")
            .expect("setting stores");

        assert_eq!(cache.clear().expect("cache clears").items, 1);
        let stats = cache.stats().expect("stats read");
        assert_eq!(stats.cache_entries(), 0);
        assert_eq!(stats.bookmarks, 1);
        assert_eq!(stats.read_items, 1);
        assert_eq!(stats.settings, 1);
    }

    #[test]
    fn concurrent_handles_do_not_replace_unrelated_read_items() {
        let directory = tempfile::tempdir().expect("tempdir creates");
        let path = directory.path().join("shared.sqlite3");
        let first = Cache::open(&path).expect("first cache opens");
        let second = Cache::open(&path).expect("second cache opens");

        first.set_read(1).expect("first process marks read");
        second.set_read(2).expect("second process marks read");
        assert_eq!(
            first.read_items().expect("combined state reads"),
            vec![1, 2]
        );

        second.remove_read(2).expect("second process marks unread");
        assert_eq!(first.read_items().expect("remaining state reads"), vec![1]);
    }

    #[test]
    fn file_path_is_explicit_and_persistent() {
        let directory = tempfile::tempdir().expect("tempdir creates");
        let path = directory.path().join("nested").join("custom.sqlite3");
        {
            let cache = Cache::open(&path).expect("cache opens");
            cache.set_setting("key", "value").expect("setting stores");
            assert_eq!(cache.path(), Some(path.as_path()));
        }
        let reopened = Cache::open(&path).expect("cache reopens");
        assert_eq!(
            reopened
                .get_setting("key")
                .expect("setting reads")
                .as_deref(),
            Some("value")
        );
    }
}
