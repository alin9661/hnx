//! Command-line parsing, cache-first orchestration, and terminal lifecycle.

use std::{
    fs::OpenOptions,
    future::Future,
    io::{BufWriter, ErrorKind, Stdout, Write, stderr, stdout},
    panic,
    path::{Path, PathBuf},
    sync::{
        Mutex, Once,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use crossterm::{
    cursor::Show,
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent,
        KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt as _;
use ratatui::{Terminal, backend::CrosstermBackend};
use serde::Serialize;
use thiserror::Error;
use tokio::{sync::mpsc, task::JoinHandle};
use tracing_subscriber::EnvFilter;

use crate::{
    HybridClient,
    api::{ApiError, SearchType},
    app::{App, AppAction, ArticleView},
    article::{Article as FetchedArticle, ArticleClient},
    cache::{Cache, CacheEntry, CacheError},
    config::{LayoutOverride, resolve_layout},
    model::{Comment, Feed, Item, Source, StoryPage, Thread},
    sanitize::{sanitize_single_line, sanitize_text, validate_url},
    theme::Theme,
    ui,
};

const SCHEMA_VERSION: u8 = 1;
const DEFAULT_LIMIT: usize = 30;
const MAX_LIMIT: usize = 500;
const MAX_SEARCH_QUERY_BYTES: usize = 4_000;
const FEED_TTL: Duration = Duration::from_secs(5 * 60);
const ITEM_TTL: Duration = Duration::from_secs(60 * 60);
const THREAD_TTL: Duration = Duration::from_secs(15 * 60);
const SEARCH_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Parser)]
#[command(
    name = "hnx",
    version,
    about = "A fast, cache-first Hacker News terminal client",
    propagate_version = true
)]
struct Cli {
    /// Never make a network request; return cached data or exit 3.
    #[arg(long, global = true)]
    offline: bool,

    /// Built-in theme name or path to a custom TOML theme.
    #[arg(long, global = true)]
    theme: Option<String>,

    /// Load layout preferences from this TOML file.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Override and save the pane layout: two[:STORIES], three[:STORIES,THREAD], or reset.
    #[arg(long, global = true, value_name = "LAYOUT")]
    layout: Option<LayoutOverride>,

    /// Write opt-in diagnostics to a local file.
    #[arg(long, global = true, value_name = "PATH")]
    log_file: Option<PathBuf>,

    /// Override the platform cache directory (useful for portable installs).
    #[arg(long, global = true, value_name = "DIR", hide = true)]
    cache_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Read a ranked Hacker News feed.
    Feed {
        feed: Feed,
        #[arg(long, default_value_t = DEFAULT_LIMIT, value_parser = parse_limit)]
        limit: usize,
        #[command(flatten)]
        output: OutputArgs,
    },
    /// Read one item, optionally including its comment tree.
    Item {
        #[arg(value_parser = parse_item_id)]
        id: u64,
        #[arg(long)]
        comments: bool,
        #[command(flatten)]
        output: OutputArgs,
    },
    /// Search stories, comments, or both through Algolia.
    Search {
        #[arg(value_parser = parse_search_query)]
        query: String,
        #[arg(long = "type", default_value = "all")]
        search_type: SearchType,
        #[arg(long, default_value_t = DEFAULT_LIMIT, value_parser = parse_limit)]
        limit: usize,
        #[command(flatten)]
        output: OutputArgs,
    },
    /// Discover current hiring threads and optionally narrow the results.
    Hiring {
        #[arg(value_parser = parse_search_query)]
        query: Option<String>,
        #[arg(long, default_value_t = DEFAULT_LIMIT, value_parser = parse_limit)]
        limit: usize,
        #[command(flatten)]
        output: OutputArgs,
    },
    /// Discover freelancer threads and optionally narrow the results.
    Freelance {
        #[arg(value_parser = parse_search_query)]
        query: Option<String>,
        #[arg(long, default_value_t = DEFAULT_LIMIT, value_parser = parse_limit)]
        limit: usize,
        #[command(flatten)]
        output: OutputArgs,
    },
    /// Inspect or maintain the local cache.
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Args)]
struct OutputArgs {
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Clone, Copy, Subcommand)]
enum CacheCommand {
    /// Show row counts and approximate payload bytes.
    Stats,
    /// Remove expired network payloads while preserving local state.
    Prune,
    /// Remove cached network payloads while preserving bookmarks/settings.
    Clear,
}

#[derive(Debug, Error)]
pub enum CliError {
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    Unavailable(String),
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error(transparent)]
    Cache(#[from] CacheError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not encode JSON output: {0}")]
    Json(#[from] serde_json::Error),
    #[error("terminal error: {0}")]
    Terminal(String),
}

impl CliError {
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::InvalidInput(_) | Self::Api(ApiError::InvalidRequest(_)) => 2,
            Self::Unavailable(_) | Self::Api(_) => 3,
            Self::Cache(_) | Self::Io(_) | Self::Json(_) | Self::Terminal(_) => 1,
        }
    }
}

#[derive(Debug, Serialize)]
struct Envelope<T> {
    schema_version: u8,
    source: Source,
    stale: bool,
    fetched_at: i64,
    data: T,
}

#[derive(Debug, Serialize)]
struct ThreadData<'a> {
    item: &'a Item,
    comments: &'a [Comment],
    metadata: crate::model::ThreadMetadata,
}

impl<T> Envelope<T> {
    const fn new(source: Source, stale: bool, fetched_at: i64, data: T) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            source,
            stale,
            fetched_at,
            data,
        }
    }
}

/// Parse arguments, execute one command, and keep stdout reserved for requested data.
///
/// # Errors
///
/// Returns a typed error when cache setup, network access, serialization, or
/// terminal initialization fails. [`CliError::exit_code`] maps it to the
/// stable process status contract.
pub async fn run() -> Result<(), CliError> {
    let cli = Cli::parse();
    init_tracing(cli.log_file.as_ref())?;
    let cache = cli
        .cache_dir
        .as_ref()
        .map_or_else(Cache::open_default, Cache::open_in_dir)?;

    match cli.command {
        None => {
            run_tui(
                cache,
                cli.offline,
                cli.theme.as_deref(),
                cli.config.as_deref(),
                cli.layout.as_ref(),
            )
            .await
        }
        Some(Command::Feed {
            feed,
            limit,
            output,
        }) => run_feed(&cache, cli.offline, feed, limit, output.format).await,
        Some(Command::Item {
            id,
            comments,
            output,
        }) => run_item(&cache, cli.offline, id, comments, output.format).await,
        Some(Command::Search {
            query,
            search_type,
            limit,
            output,
        }) => {
            run_search(
                &cache,
                cli.offline,
                &query,
                search_type,
                limit,
                output.format,
            )
            .await
        }
        Some(Command::Hiring {
            query,
            limit,
            output,
        }) => {
            let query = discovery_query("Ask HN: Who is hiring", query.as_deref());
            run_search(
                &cache,
                cli.offline,
                &query,
                SearchType::Story,
                limit,
                output.format,
            )
            .await
        }
        Some(Command::Freelance {
            query,
            limit,
            output,
        }) => {
            let query =
                discovery_query("Ask HN: Freelancer? Seeking freelancer?", query.as_deref());
            run_search(
                &cache,
                cli.offline,
                &query,
                SearchType::All,
                limit,
                output.format,
            )
            .await
        }
        Some(Command::Cache { command }) => run_cache_command(&cache, command),
    }
}

async fn run_feed(
    cache: &Cache,
    offline: bool,
    feed: Feed,
    limit: usize,
    format: OutputFormat,
) -> Result<(), CliError> {
    let cached = cache.get_feed_for_limit(feed, limit)?;
    if let Some(entry) = cached.as_ref().filter(|entry| !entry.metadata.stale) {
        return print_page(cache_page(entry.clone(), Some(limit)), format);
    }
    if offline {
        return cached.map_or_else(
            || unavailable(format!("no cached {feed} feed is available offline")),
            |entry| print_page(cache_page(entry, Some(limit)), format),
        );
    }

    match HybridClient::new().feed(feed, limit).await {
        Ok(page) => {
            if let Err(error) = cache.put_feed(&page, FEED_TTL) {
                warn_stderr(format_args!("could not cache feed: {error}"));
            }
            print_page(page, format)
        }
        Err(error) => cached.map_or_else(
            || Err(CliError::Api(error)),
            |entry| print_page(cache_page(entry, Some(limit)), format),
        ),
    }
}

async fn run_item(
    cache: &Cache,
    offline: bool,
    id: u64,
    comments: bool,
    format: OutputFormat,
) -> Result<(), CliError> {
    if comments {
        let cached = cache.get_thread(id)?;
        if let Some(entry) = cached.as_ref().filter(|entry| !entry.metadata.stale) {
            return print_thread_entry(entry.clone(), format);
        }
        if offline {
            return cached.map_or_else(
                || unavailable(format!("thread {id} is not cached")),
                |entry| print_thread_entry(entry, format),
            );
        }
        return match HybridClient::new().thread(id).await {
            Ok(thread) => {
                if let Err(error) = cache.put_thread(&thread, THREAD_TTL) {
                    warn_stderr(format_args!("could not cache thread: {error}"));
                }
                print_thread(thread, format)
            }
            Err(error) => cached.map_or_else(
                || Err(CliError::Api(error)),
                |entry| print_thread_entry(entry, format),
            ),
        };
    }

    let cached = cache.get_item(id)?;
    if let Some(entry) = cached.as_ref().filter(|entry| !entry.metadata.stale) {
        return print_item_entry(entry.clone(), format);
    }
    if offline {
        return cached.map_or_else(
            || unavailable(format!("item {id} is not cached")),
            |entry| print_item_entry(entry, format),
        );
    }
    match HybridClient::new().item_with_source(id).await {
        Ok((item, source)) => {
            if let Err(error) = cache.put_item(&item, ITEM_TTL) {
                warn_stderr(format_args!("could not cache item: {error}"));
            }
            print_item(item, source, false, unix_timestamp(), format)
        }
        Err(error) => cached.map_or_else(
            || Err(CliError::Api(error)),
            |entry| print_item_entry(entry, format),
        ),
    }
}

async fn run_search(
    cache: &Cache,
    offline: bool,
    query: &str,
    search_type: SearchType,
    limit: usize,
    format: OutputFormat,
) -> Result<(), CliError> {
    let query = parse_search_query(query).map_err(CliError::InvalidInput)?;
    let cache_key = format!("{}:{query}", search_type.as_str());
    let cached = cache.get_search_for_limit(&cache_key, limit)?;
    if let Some(entry) = cached.as_ref().filter(|entry| !entry.metadata.stale) {
        return print_page(cache_page(entry.clone(), Some(limit)), format);
    }
    if offline {
        return cached.map_or_else(
            || unavailable(format!("search `{query}` is not cached")),
            |entry| print_page(cache_page(entry, Some(limit)), format),
        );
    }

    match HybridClient::new().search(&query, search_type, limit).await {
        Ok(page) => {
            if let Err(error) = cache.put_search(&cache_key, &page, SEARCH_TTL) {
                warn_stderr(format_args!("could not cache search results: {error}"));
            }
            print_page(page, format)
        }
        Err(error) => cached.map_or_else(
            || Err(CliError::Api(error)),
            |entry| print_page(cache_page(entry, Some(limit)), format),
        ),
    }
}

fn run_cache_command(cache: &Cache, command: CacheCommand) -> Result<(), CliError> {
    match command {
        CacheCommand::Stats => {
            let stats = cache.stats()?;
            let schema_version = cache.schema_version()?;
            let path = cache
                .path()
                .map_or_else(|| "<memory>".to_owned(), |path| path.display().to_string());
            write_stdout(|writer| {
                writeln!(writer, "path: {path}")?;
                writeln!(writer, "schema: {schema_version}")?;
                writeln!(writer, "feeds: {}", stats.feeds)?;
                writeln!(writer, "items: {}", stats.items)?;
                writeln!(writer, "threads: {}", stats.threads)?;
                writeln!(writer, "searches: {}", stats.searches)?;
                writeln!(writer, "bookmarks: {}", stats.bookmarks)?;
                writeln!(writer, "settings: {}", stats.settings)?;
                writeln!(writer, "stale entries: {}", stats.stale_entries)?;
                writeln!(writer, "payload bytes: {}", stats.payload_bytes)?;
                writeln!(writer, "database bytes: {}", stats.database_bytes)?;
                Ok(())
            })
        }
        CacheCommand::Prune => {
            let stats = cache.prune()?;
            write_stdout(|writer| {
                writeln!(writer, "pruned {} expired cache rows", stats.total())?;
                Ok(())
            })
        }
        CacheCommand::Clear => {
            cache.clear()?;
            write_stdout(|writer| {
                writeln!(
                    writer,
                    "cleared network cache; bookmarks and settings were preserved"
                )?;
                Ok(())
            })
        }
    }
}

fn print_page(page: StoryPage, format: OutputFormat) -> Result<(), CliError> {
    write_stdout(move |writer| write_page(writer, &page, format))
}

fn write_page(
    writer: &mut dyn Write,
    page: &StoryPage,
    format: OutputFormat,
) -> Result<(), CliError> {
    match format {
        OutputFormat::Json => write_json(
            writer,
            &Envelope::new(page.source, page.stale, page.fetched_at, &page.items),
        ),
        OutputFormat::Text => {
            write_stale_marker(writer, page.source, page.stale, page.fetched_at)?;
            for (index, item) in page.items.iter().enumerate() {
                writeln!(writer, "{}", story_line(index + 1, item))?;
            }
            Ok(())
        }
    }
}

fn print_item(
    item: Item,
    source: Source,
    stale: bool,
    fetched_at: i64,
    format: OutputFormat,
) -> Result<(), CliError> {
    write_stdout(move |writer| write_item(writer, &item, source, stale, fetched_at, format))
}

fn write_item(
    writer: &mut dyn Write,
    item: &Item,
    source: Source,
    stale: bool,
    fetched_at: i64,
    format: OutputFormat,
) -> Result<(), CliError> {
    match format {
        OutputFormat::Json => write_json(writer, &Envelope::new(source, stale, fetched_at, item)),
        OutputFormat::Text => {
            write_stale_marker(writer, source, stale, fetched_at)?;
            writeln!(writer, "{}", item_text(item))?;
            Ok(())
        }
    }
}

fn print_item_entry(entry: CacheEntry<Item>, format: OutputFormat) -> Result<(), CliError> {
    print_item(
        entry.value,
        Source::Cache,
        entry.metadata.stale,
        entry.metadata.fetched_at,
        format,
    )
}

fn print_thread(thread: Thread, format: OutputFormat) -> Result<(), CliError> {
    write_stdout(move |writer| write_thread(writer, &thread, format))
}

fn write_thread(
    writer: &mut dyn Write,
    thread: &Thread,
    format: OutputFormat,
) -> Result<(), CliError> {
    match format {
        OutputFormat::Json => {
            let data = ThreadData {
                item: &thread.item,
                comments: &thread.comments,
                metadata: thread.metadata(),
            };
            write_json(
                writer,
                &Envelope::new(thread.source, thread.stale, thread.fetched_at, data),
            )
        }
        OutputFormat::Text => {
            write_stale_marker(writer, thread.source, thread.stale, thread.fetched_at)?;
            let metadata = thread.metadata();
            if metadata.partial {
                writeln!(
                    writer,
                    "[partial thread · loaded {} of {} comments · omitted {} · unresolved {}]",
                    metadata.loaded_comments,
                    metadata.declared_comments,
                    metadata.omitted_comments,
                    metadata.unresolved_ids.len()
                )?;
            }
            writeln!(writer, "{}", item_text(&thread.item))?;
            write_comments(writer, &thread.comments)?;
            Ok(())
        }
    }
}

fn print_thread_entry(entry: CacheEntry<Thread>, format: OutputFormat) -> Result<(), CliError> {
    let mut thread = entry.value;
    thread.source = Source::Cache;
    thread.stale = entry.metadata.stale;
    thread.fetched_at = entry.metadata.fetched_at;
    print_thread(thread, format)
}

fn write_stdout(
    operation: impl FnOnce(&mut dyn Write) -> Result<(), CliError>,
) -> Result<(), CliError> {
    let output = stdout();
    write_buffered(output.lock(), operation)
}

fn write_buffered(
    output: impl Write,
    operation: impl FnOnce(&mut dyn Write) -> Result<(), CliError>,
) -> Result<(), CliError> {
    let mut writer = BufWriter::new(output);
    let result = operation(&mut writer).and_then(|()| writer.flush().map_err(CliError::Io));
    match result {
        Err(error) if is_broken_pipe(&error) => Ok(()),
        other => other,
    }
}

fn is_broken_pipe(error: &CliError) -> bool {
    match error {
        CliError::Io(error) => error.kind() == ErrorKind::BrokenPipe,
        CliError::Json(error) => error.io_error_kind() == Some(ErrorKind::BrokenPipe),
        _ => false,
    }
}

fn write_json(writer: &mut dyn Write, value: &impl Serialize) -> Result<(), CliError> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn write_stale_marker(
    writer: &mut dyn Write,
    source: Source,
    stale: bool,
    fetched_at: i64,
) -> Result<(), CliError> {
    if stale {
        writeln!(writer, "[stale {source} · fetched_at {fetched_at}]")?;
    }
    Ok(())
}

fn warn_stderr(message: std::fmt::Arguments<'_>) {
    let _ = writeln!(stderr().lock(), "hnx: warning: {message}");
}

fn cache_page(mut entry: CacheEntry<StoryPage>, limit: Option<usize>) -> StoryPage {
    if let Some(limit) = limit {
        entry.value.items.truncate(limit);
    }
    entry.value.source = Source::Cache;
    entry.value.stale = entry.metadata.stale;
    entry.value.fetched_at = entry.metadata.fetched_at;
    entry.value
}

fn story_line(rank: usize, item: &Item) -> String {
    let title = sanitize_single_line(item.display_title());
    let author = sanitize_single_line(item.by.as_deref().unwrap_or("unknown"));
    let suffix = item
        .url
        .as_deref()
        .and_then(|value| url::Url::parse(value).ok())
        .map_or_else(String::new, |url| {
            url.host_str()
                .map_or_else(String::new, |host| format!(" ({host})"))
        });
    format!(
        "{rank}. {title}{suffix} — {} points, {} comments, by {author} [id:{}]",
        item.score, item.descendants, item.id
    )
}

fn item_text(item: &Item) -> String {
    let mut output = format!(
        "{}\n{} points, {} comments, by {} [id:{}]",
        sanitize_single_line(item.display_title()),
        item.score,
        item.descendants,
        sanitize_single_line(item.by.as_deref().unwrap_or("unknown")),
        item.id
    );
    if let Some(url) = &item.url {
        output.push('\n');
        output.push_str(&sanitize_single_line(url));
    }
    if let Some(text) = &item.text {
        output.push('\n');
        output.push_str(&sanitize_text(text));
    }
    output
}

fn write_comments(writer: &mut dyn Write, comments: &[Comment]) -> Result<(), CliError> {
    let mut pending: Vec<_> = comments.iter().rev().collect();
    while let Some(comment) = pending.pop() {
        let indent = "  ".repeat(usize::try_from(comment.depth).unwrap_or(usize::MAX).min(32));
        let author = sanitize_single_line(comment.by.as_deref().unwrap_or("unknown"));
        writeln!(writer, "{indent}{author} [id:{}]", comment.id)?;
        if let Some(text) = &comment.text {
            for line in sanitize_text(text).lines() {
                writeln!(writer, "{indent}  {line}")?;
            }
        }
        pending.extend(comment.children.iter().rev());
    }
    Ok(())
}

fn discovery_query(prefix: &str, query: Option<&str>) -> String {
    query.map_or_else(
        || prefix.to_owned(),
        |query| format!("{prefix} {}", query.trim()),
    )
}

fn parse_limit(value: &str) -> Result<usize, String> {
    let limit = value
        .parse::<usize>()
        .map_err(|_| "limit must be an integer".to_owned())?;
    (1..=MAX_LIMIT)
        .contains(&limit)
        .then_some(limit)
        .ok_or_else(|| format!("limit must be between 1 and {MAX_LIMIT}"))
}

fn parse_item_id(value: &str) -> Result<u64, String> {
    let id = value
        .parse::<u64>()
        .map_err(|_| "item id must be a positive integer".to_owned())?;
    (id > 0)
        .then_some(id)
        .ok_or_else(|| "item id must be greater than zero".to_owned())
}

fn parse_search_query(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("search query must not be empty".to_owned());
    }
    if value.len() > MAX_SEARCH_QUERY_BYTES {
        return Err(format!(
            "search query must be at most {MAX_SEARCH_QUERY_BYTES} bytes"
        ));
    }
    if value.chars().any(char::is_control) {
        return Err("search query must not contain control characters".to_owned());
    }
    Ok(value.to_owned())
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

fn unavailable<T>(message: String) -> Result<T, CliError> {
    Err(CliError::Unavailable(message))
}

fn init_tracing(path: Option<&PathBuf>) -> Result<(), CliError> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("hnx=info"));
    if let Some(path) = path {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(Mutex::new(file))
            .try_init()
            .map_err(|error| CliError::Terminal(format!("could not initialize logging: {error}")))
    } else if std::env::var_os("RUST_LOG").is_some() {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(stderr)
            .try_init()
            .map_err(|error| CliError::Terminal(format!("could not initialize logging: {error}")))
    } else {
        Ok(())
    }
}

#[derive(Debug)]
enum UiMessage {
    Page {
        request_id: u64,
        context: PageContext,
        result: Result<StoryPage, String>,
    },
    Thread {
        request_id: u64,
        item_id: u64,
        result: Result<Thread, String>,
    },
    Article {
        request_id: u64,
        item_id: u64,
        title: String,
        result: Result<FetchedArticle, String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PageContext {
    Feed(Feed),
    Search(String),
}

impl PageContext {
    fn matches(&self, app: &App) -> bool {
        match self {
            Self::Feed(feed) => app.feed() == *feed && app.search_query().is_none(),
            Self::Search(query) => app.search_query() == Some(query.as_str()),
        }
    }
}

static TERMINAL_RESTORE_NEEDED: AtomicBool = AtomicBool::new(false);
static INSTALL_TERMINAL_PANIC_HOOK: Once = Once::new();

struct CleanupGuard<F: FnOnce()> {
    cleanup: Option<F>,
}

impl<F: FnOnce()> CleanupGuard<F> {
    const fn new(cleanup: F) -> Self {
        Self {
            cleanup: Some(cleanup),
        }
    }

    fn disarm(mut self) {
        self.cleanup = None;
    }
}

impl<F: FnOnce()> Drop for CleanupGuard<F> {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
    }
}

fn install_terminal_panic_hook() {
    INSTALL_TERMINAL_PANIC_HOOK.call_once(|| {
        let previous_hook = panic::take_hook();
        panic::set_hook(Box::new(move |panic_info| {
            // Hooks run before either unwinding or `panic = "abort"`; cleanup
            // therefore cannot depend on `TerminalSession::drop` alone.
            restore_terminal();
            previous_hook(panic_info);
        }));
    });
}

fn restore_terminal() {
    cleanup_once(&TERMINAL_RESTORE_NEEDED, restore_terminal_best_effort);
}

fn cleanup_once(active: &AtomicBool, cleanup: impl FnOnce()) {
    if active.swap(false, Ordering::AcqRel) {
        cleanup();
    }
}

fn restore_terminal_best_effort() {
    let mut output = stdout();
    let _ = execute!(output, DisableMouseCapture);
    let _ = execute!(output, LeaveAlternateScreen);
    let _ = execute!(output, Show);
    let _ = disable_raw_mode();
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalSession {
    fn new() -> Result<Self, CliError> {
        install_terminal_panic_hook();
        TERMINAL_RESTORE_NEEDED.store(true, Ordering::Release);
        let cleanup = CleanupGuard::new(restore_terminal);

        enable_raw_mode()?;
        let mut output = stdout();
        execute!(output, EnterAlternateScreen)?;
        execute!(output, EnableMouseCapture)?;
        let terminal = Terminal::new(CrosstermBackend::new(output))?;

        cleanup.disarm();
        Ok(Self { terminal })
    }

    fn draw(&mut self, app: &mut App, theme: &Theme) -> Result<(), CliError> {
        self.terminal.draw(|frame| ui::render(frame, app, theme))?;
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        restore_terminal();
    }
}

#[cfg(unix)]
fn termination_signal() -> impl Future<Output = ()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate());
    let mut hangup = signal(SignalKind::hangup());
    if let Err(error) = &terminate {
        tracing::debug!(%error, "SIGTERM listener unavailable");
    }
    if let Err(error) = &hangup {
        tracing::debug!(%error, "SIGHUP listener unavailable");
    }

    async move {
        tokio::select! {
            () = async {
                match &mut terminate {
                    Ok(signal) => {
                        let _ = signal.recv().await;
                    }
                    Err(_) => std::future::pending().await,
                }
            } => {}
            () = async {
                match &mut hangup {
                    Ok(signal) => {
                        let _ = signal.recv().await;
                    }
                    Err(_) => std::future::pending().await,
                }
            } => {}
        }
    }
}

#[cfg(windows)]
fn termination_signal() -> impl Future<Output = ()> {
    use tokio::signal::windows::{ctrl_break, ctrl_close, ctrl_logoff, ctrl_shutdown};

    let mut ctrl_break = ctrl_break();
    let mut ctrl_close = ctrl_close();
    let mut ctrl_logoff = ctrl_logoff();
    let mut ctrl_shutdown = ctrl_shutdown();
    for (name, listener) in [
        ("CTRL_BREAK", ctrl_break.as_ref().err()),
        ("CTRL_CLOSE", ctrl_close.as_ref().err()),
        ("CTRL_LOGOFF", ctrl_logoff.as_ref().err()),
        ("CTRL_SHUTDOWN", ctrl_shutdown.as_ref().err()),
    ] {
        if let Some(error) = listener {
            tracing::debug!(%error, signal = name, "termination listener unavailable");
        }
    }

    async move {
        tokio::select! {
            () = async {
                match &mut ctrl_break {
                    Ok(signal) => {
                        let _ = signal.recv().await;
                    }
                    Err(_) => std::future::pending().await,
                }
            } => {}
            () = async {
                match &mut ctrl_close {
                    Ok(signal) => {
                        let _ = signal.recv().await;
                    }
                    Err(_) => std::future::pending().await,
                }
            } => {}
            () = async {
                match &mut ctrl_logoff {
                    Ok(signal) => {
                        let _ = signal.recv().await;
                    }
                    Err(_) => std::future::pending().await,
                }
            } => {}
            () = async {
                match &mut ctrl_shutdown {
                    Ok(signal) => {
                        let _ = signal.recv().await;
                    }
                    Err(_) => std::future::pending().await,
                }
            } => {}
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn termination_signal() -> impl Future<Output = ()> {
    std::future::pending()
}

#[allow(clippy::too_many_lines)]
async fn run_tui(
    cache: Cache,
    offline: bool,
    requested_theme: Option<&str>,
    config_path: Option<&Path>,
    layout_override: Option<&LayoutOverride>,
) -> Result<(), CliError> {
    let (theme, theme_warning) = resolve_theme(&cache, requested_theme);
    let (stored_layout, stored_read_warning) = match cache.get_setting("layout.v1") {
        Ok(value) => (value, None),
        Err(_) => (
            None,
            Some("Could not read saved layout; using config or built-in defaults".to_owned()),
        ),
    };
    let layout = resolve_layout(config_path, stored_layout.as_deref(), layout_override)
        .map_err(|error| CliError::InvalidInput(error.to_string()))?;
    let mut layout_warning = stored_read_warning.or(layout.warning.clone());
    if layout.reset_saved {
        if cache.remove_setting("layout.v1").is_err() {
            layout_warning.get_or_insert_with(|| {
                "Layout reset, but the saved override could not be removed".to_owned()
            });
        }
    } else if layout.persist_cli && cache.set_json_setting("layout.v1", &layout.active).is_err() {
        layout_warning.get_or_insert_with(|| {
            "Layout applied, but the preference could not be saved".to_owned()
        });
    }
    let cached = cache.get_feed_for_limit(Feed::Top, DEFAULT_LIMIT)?;
    let had_cached_page = cached.is_some();
    let mut app = cached.map_or_else(
        || App::empty(Feed::Top),
        |entry| App::new(cache_page(entry, Some(DEFAULT_LIMIT))),
    );
    app.configure_layout(layout.active, layout.baseline);
    app.set_bookmarks(cache.bookmarks()?.into_iter().map(|item| item.id));
    if offline {
        app.set_offline(true);
        if !had_cached_page {
            app.set_error("No cached top feed is available offline");
        }
    }
    if let Some(warning) = theme_warning.or(layout_warning)
        && app.error().is_none()
    {
        app.set_status(warning);
    }

    let client = HybridClient::new();
    let article_client = ArticleClient::new().map_err(|error| {
        CliError::Terminal(format!("could not initialize article reader: {error}"))
    })?;
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let mut page_request_id = 0_u64;
    let mut thread_request_id = 0_u64;
    let mut article_request_id = 0_u64;
    let mut page_task: Option<JoinHandle<()>> = None;
    let mut thread_task: Option<JoinHandle<()>> = None;
    let mut article_task: Option<JoinHandle<()>> = None;

    if !offline {
        page_request_id = next_request_id(page_request_id);
        app.set_loading(true);
        page_task = Some(spawn_feed(
            sender.clone(),
            client.clone(),
            cache.clone(),
            Feed::Top,
            DEFAULT_LIMIT,
            page_request_id,
        ));
    }

    let termination = termination_signal();
    tokio::pin!(termination);
    let mut terminal = TerminalSession::new()?;
    terminal.draw(&mut app, &theme)?;
    let mut events = EventStream::new();
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    let mut quit = false;

    while !quit {
        let mut redraw = false;
        tokio::select! {
            event = events.next() => {
                match event {
                    Some(Ok(Event::Resize(_, _) | Event::FocusGained | Event::FocusLost)) => {
                        redraw = true;
                    }
                    Some(Ok(Event::Paste(text))) => {
                        if app.prompt().is_some() {
                            for character in text.chars().filter(|character| !character.is_control()) {
                                let _ = app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
                            }
                        }
                        redraw = true;
                    }
                    Some(Ok(event)) => {
                        let key = match event {
                            Event::Key(key) => Some(key),
                            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollUp => {
                                Some(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
                            }
                            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollDown => {
                                Some(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
                            }
                            _ => None,
                        };
                        if let Some(key) = key {
                            let selected_before = app.selected_item().map(|item| item.id);
                            let action = app.handle_key(key);
                            let selected_after = app.selected_item().map(|item| item.id);
                            if selected_before != selected_after {
                                thread_request_id = next_request_id(thread_request_id);
                                article_request_id = next_request_id(article_request_id);
                                abort_task(&mut thread_task);
                                abort_task(&mut article_task);
                            }

                            match action {
                                AppAction::None => {}
                                AppAction::Quit => quit = true,
                                AppAction::LoadFeed(feed) => {
                                    page_request_id = next_request_id(page_request_id);
                                    thread_request_id = next_request_id(thread_request_id);
                                    article_request_id = next_request_id(article_request_id);
                                    abort_task(&mut page_task);
                                    abort_task(&mut thread_task);
                                    abort_task(&mut article_task);
                                    let cached = cache.get_feed_for_limit(feed, DEFAULT_LIMIT)?;
                                    let had_cache = cached.is_some();
                                    if let Some(entry) = cached {
                                        app.load_page(cache_page(entry, Some(DEFAULT_LIMIT)));
                                    }
                                    if app.offline() {
                                        if !had_cache {
                                            app.set_error(format!("No cached {feed} feed is available offline"));
                                        }
                                    } else {
                                        app.set_loading(true);
                                        page_task = Some(spawn_feed(
                                            sender.clone(), client.clone(), cache.clone(), feed,
                                            DEFAULT_LIMIT, page_request_id,
                                        ));
                                    }
                                }
                                AppAction::Search(query) => {
                                    page_request_id = next_request_id(page_request_id);
                                    thread_request_id = next_request_id(thread_request_id);
                                    article_request_id = next_request_id(article_request_id);
                                    abort_task(&mut page_task);
                                    abort_task(&mut thread_task);
                                    abort_task(&mut article_task);
                                    if let Err(error) = parse_search_query(&query) {
                                        app.set_error(error);
                                    } else {
                                        let query = query.trim().to_owned();
                                        let key = tui_search_key(&query);
                                        let cached = cache.get_search_for_limit(&key, DEFAULT_LIMIT)?;
                                        let had_cache = cached.is_some();
                                        if let Some(entry) = cached {
                                            app.load_page(cache_page(entry, Some(DEFAULT_LIMIT)));
                                        }
                                        if app.offline() {
                                            if !had_cache {
                                                app.set_error(format!("Search `{query}` is not cached"));
                                            }
                                        } else {
                                            app.set_loading(true);
                                            page_task = Some(spawn_search(
                                                sender.clone(), client.clone(), cache.clone(), query,
                                                DEFAULT_LIMIT, page_request_id,
                                            ));
                                        }
                                    }
                                }
                                AppAction::Refresh => {
                                    page_request_id = next_request_id(page_request_id);
                                    abort_task(&mut page_task);
                                    if app.offline() {
                                        app.set_error("Refresh is unavailable in offline mode");
                                    } else {
                                        app.set_loading(true);
                                        page_task = Some(if let Some(query) = app.search_query().map(str::to_owned) {
                                            spawn_search(
                                                sender.clone(), client.clone(), cache.clone(), query,
                                                DEFAULT_LIMIT, page_request_id,
                                            )
                                        } else {
                                            spawn_feed(
                                                sender.clone(), client.clone(), cache.clone(), app.feed(),
                                                DEFAULT_LIMIT, page_request_id,
                                            )
                                        });
                                    }
                                }
                                AppAction::LoadThread(item_id) => {
                                    thread_request_id = next_request_id(thread_request_id);
                                    article_request_id = next_request_id(article_request_id);
                                    abort_task(&mut thread_task);
                                    abort_task(&mut article_task);
                                    let cached = cache.get_thread(item_id)?;
                                    let had_cache = cached.is_some();
                                    if let Some(entry) = cached {
                                        app.load_thread(cache_thread(entry));
                                    }
                                    if app.offline() {
                                        if !had_cache {
                                            app.set_error(format!("Thread {item_id} is not cached"));
                                        }
                                    } else {
                                        app.set_loading(true);
                                        thread_task = Some(spawn_thread(
                                            sender.clone(), client.clone(), cache.clone(), item_id, thread_request_id,
                                        ));
                                    }
                                }
                                AppAction::LoadArticle(item_id) => {
                                    article_request_id = next_request_id(article_request_id);
                                    abort_task(&mut article_task);
                                    if app.offline() {
                                        app.set_error("Article fetching is unavailable in offline mode");
                                    } else if let Some(item) = app.stories().iter().find(|item| item.id == item_id) {
                                        if let Some(url) = item.url.clone() {
                                            article_task = Some(spawn_article(
                                                sender.clone(), article_client.clone(), item_id,
                                                sanitize_single_line(item.display_title()), url, article_request_id,
                                            ));
                                        } else {
                                            app.set_error("This item does not link to an article");
                                        }
                                    }
                                }
                                AppAction::OpenStory(item_id) => handle_open_story(&mut app, item_id),
                                AppAction::SetOffline(is_offline) => {
                                    if is_offline {
                                        page_request_id = next_request_id(page_request_id);
                                        thread_request_id = next_request_id(thread_request_id);
                                        article_request_id = next_request_id(article_request_id);
                                        abort_task(&mut page_task);
                                        abort_task(&mut thread_task);
                                        abort_task(&mut article_task);
                                    } else {
                                        page_request_id = next_request_id(page_request_id);
                                        app.set_loading(true);
                                        page_task = Some(if let Some(query) = app.search_query().map(str::to_owned) {
                                            spawn_search(
                                                sender.clone(), client.clone(), cache.clone(), query,
                                                DEFAULT_LIMIT, page_request_id,
                                            )
                                        } else {
                                            spawn_feed(
                                                sender.clone(), client.clone(), cache.clone(), app.feed(),
                                                DEFAULT_LIMIT, page_request_id,
                                            )
                                        });
                                    }
                                }
                                AppAction::BookmarkChanged { item_id, bookmarked } => {
                                    let result = if bookmarked {
                                        app.stories()
                                            .iter()
                                            .find(|item| item.id == item_id)
                                            .ok_or_else(|| CacheError::InvalidKey("selected bookmark item is missing"))
                                            .and_then(|item| cache.set_bookmark(item))
                                    } else {
                                        cache.remove_bookmark(item_id).map(|_| ())
                                    };
                                    if let Err(error) = result {
                                        app.set_bookmarked(item_id, !bookmarked);
                                        app.set_error(format!("Could not update bookmark: {error}"));
                                    }
                                }
                                AppAction::LayoutChanged(layout) => {
                                    if let Err(error) = cache.set_json_setting("layout.v1", &layout) {
                                        app.set_status(format!("Layout changed but was not saved: {error}"));
                                    }
                                }
                                AppAction::LayoutReset => {
                                    if let Err(error) = cache.remove_setting("layout.v1") {
                                        app.set_status(format!("Layout reset but saved override remains: {error}"));
                                    }
                                }
                            }
                            redraw = true;
                        }
                    }
                    Some(Err(error)) => return Err(CliError::Terminal(error.to_string())),
                    None => quit = true,
                }
            }
            message = receiver.recv() => {
                if let Some(message) = message {
                    match message {
                        UiMessage::Page { request_id, context, result }
                            if request_id == page_request_id && context.matches(&app) => {
                                page_task.take();
                                match result {
                                    Ok(page) => app.refresh_page(page),
                                    Err(error) => app.set_error(error),
                                }
                            }
                        UiMessage::Thread { request_id, item_id, result }
                            if request_id == thread_request_id
                                && app.selected_item().is_some_and(|item| item.id == item_id) => {
                                thread_task.take();
                                match result {
                                    Ok(thread) => app.load_thread(thread),
                                    Err(error) => app.set_error(error),
                                }
                            }
                        UiMessage::Article {
                            request_id,
                            item_id,
                            title,
                            result,
                        } if request_id == article_request_id
                            && app.selected_item().is_some_and(|item| item.id == item_id) => {
                            article_task.take();
                            match result {
                                Ok(article) => app.set_article(ArticleView::new(
                                    title,
                                    Some(article.url.to_string()),
                                    article.text,
                                )),
                                Err(error) => app.set_error(error),
                            }
                        }
                        _ => {}
                    }
                    redraw = true;
                }
            }
            result = &mut shutdown => {
                if let Err(error) = result {
                    tracing::debug!(%error, "Ctrl-C listener stopped");
                }
                quit = true;
            }
            () = &mut termination => {
                quit = true;
            }
        }

        if redraw && !quit {
            terminal.draw(&mut app, &theme)?;
        }
    }

    abort_task(&mut page_task);
    abort_task(&mut thread_task);
    abort_task(&mut article_task);
    Ok(())
}

fn spawn_feed(
    sender: mpsc::UnboundedSender<UiMessage>,
    client: HybridClient,
    cache: Cache,
    feed: Feed,
    limit: usize,
    request_id: u64,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let result = client
            .feed(feed, limit)
            .await
            .map_err(|error| error.to_string());
        if let Ok(page) = &result
            && let Err(error) = cache.put_feed(page, FEED_TTL)
        {
            tracing::warn!(%error, "could not cache refreshed feed");
        }
        let _ = sender.send(UiMessage::Page {
            request_id,
            context: PageContext::Feed(feed),
            result,
        });
    })
}

fn spawn_search(
    sender: mpsc::UnboundedSender<UiMessage>,
    client: HybridClient,
    cache: Cache,
    query: String,
    limit: usize,
    request_id: u64,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let result = client
            .search(&query, SearchType::All, limit)
            .await
            .map_err(|error| error.to_string());
        if let Ok(page) = &result
            && let Err(error) = cache.put_search(&tui_search_key(&query), page, SEARCH_TTL)
        {
            tracing::warn!(%error, "could not cache search results");
        }
        let _ = sender.send(UiMessage::Page {
            request_id,
            context: PageContext::Search(query),
            result,
        });
    })
}

fn spawn_thread(
    sender: mpsc::UnboundedSender<UiMessage>,
    client: HybridClient,
    cache: Cache,
    item_id: u64,
    request_id: u64,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let result = client
            .thread(item_id)
            .await
            .map_err(|error| error.to_string());
        if let Ok(thread) = &result
            && let Err(error) = cache.put_thread(thread, THREAD_TTL)
        {
            tracing::warn!(%error, "could not cache thread");
        }
        let _ = sender.send(UiMessage::Thread {
            request_id,
            item_id,
            result,
        });
    })
}

fn spawn_article(
    sender: mpsc::UnboundedSender<UiMessage>,
    client: ArticleClient,
    item_id: u64,
    title: String,
    url: String,
    request_id: u64,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let result = client.fetch(&url).await.map_err(|error| error.to_string());
        let _ = sender.send(UiMessage::Article {
            request_id,
            item_id,
            title,
            result,
        });
    })
}

fn cache_thread(mut entry: CacheEntry<Thread>) -> Thread {
    entry.value.source = Source::Cache;
    entry.value.stale = entry.metadata.stale;
    entry.value.fetched_at = entry.metadata.fetched_at;
    entry.value
}

fn tui_search_key(query: &str) -> String {
    format!("{}:{}", SearchType::All.as_str(), query.trim())
}

fn next_request_id(request_id: u64) -> u64 {
    request_id.wrapping_add(1).max(1)
}

fn abort_task(task: &mut Option<JoinHandle<()>>) {
    if let Some(task) = task.take() {
        task.abort();
    }
}

fn open_story(app: &mut App, item_id: u64) {
    let item = app.stories().iter().find(|item| item.id == item_id);
    let external = item
        .and_then(|item| item.url.as_deref())
        .and_then(|url| validate_url(url).ok());
    let target = external.map_or_else(
        || format!("https://news.ycombinator.com/item?id={item_id}"),
        |url| url.to_string(),
    );
    if let Err(error) = open::that(&target) {
        app.set_error(format!("Could not open browser: {error}"));
    } else {
        app.set_status(format!("Opened {target}"));
    }
}

fn handle_open_story(app: &mut App, item_id: u64) {
    if app.offline() {
        app.set_error("Opening URLs is unavailable in offline mode");
    } else {
        open_story(app, item_id);
    }
}

fn resolve_theme(cache: &Cache, requested: Option<&str>) -> (Theme, Option<String>) {
    if std::env::var_os("NO_COLOR").is_some() {
        return (Theme::no_color(), None);
    }

    let stored = if requested.is_some() {
        None
    } else {
        match cache.get_setting("theme") {
            Ok(stored) => stored,
            Err(error) => {
                return (
                    Theme::classic(),
                    Some(format!(
                        "Could not read theme setting; using classic: {error}"
                    )),
                );
            }
        }
    };
    let choice = requested.or(stored.as_deref()).unwrap_or("classic");
    let result = Theme::named(choice).or_else(|builtin_error| {
        if Path::new(choice).is_file() {
            Theme::load(choice)
        } else {
            Err(builtin_error)
        }
    });
    match result {
        Ok(theme) => {
            let warning = requested.and_then(|value| {
                let value = Theme::named(value).map_or_else(
                    |_| {
                        std::fs::canonicalize(value)
                            .unwrap_or_else(|_| PathBuf::from(value))
                            .display()
                            .to_string()
                    },
                    |_| value.to_owned(),
                );
                cache
                    .set_setting("theme", &value)
                    .err()
                    .map(|error| format!("Theme loaded but preference was not saved: {error}"))
            });
            (theme, warning)
        }
        Err(error) => (
            Theme::classic(),
            Some(format!("Invalid theme `{choice}`; using classic: {error}")),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        io::{self, ErrorKind, Write},
        mem::ManuallyDrop,
        sync::atomic::AtomicBool,
    };

    use clap::Parser as _;

    use super::{
        CleanupGuard, Cli, CliError, Command, OutputFormat, PageContext, cleanup_once,
        handle_open_story, next_request_id, parse_item_id, parse_limit, parse_search_query,
        write_buffered, write_comments, write_item, write_json, write_page, write_thread,
    };
    use crate::{
        api::SearchType,
        app::App,
        model::{Comment, Feed, Item, Source, StoryPage, Thread},
    };

    struct ErrorWriter {
        kind: ErrorKind,
    }

    impl Write for ErrorWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(self.kind, "injected output failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn parses_stable_headless_commands() {
        let cli = Cli::try_parse_from([
            "hnx",
            "--offline",
            "feed",
            "best",
            "--limit",
            "42",
            "--format",
            "json",
        ])
        .expect("feed command parses");
        assert!(cli.offline);
        let Some(Command::Feed {
            feed,
            limit,
            output,
        }) = cli.command
        else {
            panic!("expected feed command");
        };
        assert_eq!(feed, Feed::Best);
        assert_eq!(limit, 42);
        assert!(matches!(output.format, OutputFormat::Json));

        let cli = Cli::try_parse_from(["hnx", "search", "rust terminal", "--type", "comment"])
            .expect("search command parses");
        assert!(matches!(
            cli.command,
            Some(Command::Search {
                search_type: SearchType::Comment,
                ..
            })
        ));
    }

    #[test]
    fn parses_layout_and_config_options_with_clap_validation() {
        let cli =
            Cli::try_parse_from(["hnx", "--config", "custom.toml", "--layout", "three:38,34"])
                .expect("layout options parse");
        assert_eq!(
            cli.config.as_deref(),
            Some(std::path::Path::new("custom.toml"))
        );
        assert!(matches!(
            cli.layout,
            Some(crate::config::LayoutOverride::Apply {
                mode: crate::layout::PaneMode::Three,
                ..
            })
        ));
        assert!(Cli::try_parse_from(["hnx", "--layout", "two:10"]).is_err());
        assert!(Cli::try_parse_from(["hnx", "--layout", "three:50,60"]).is_err());
    }

    #[test]
    fn numeric_bounds_are_explicit() {
        assert!(parse_item_id("0").is_err());
        assert!(parse_limit("0").is_err());
        assert!(parse_limit("501").is_err());
        assert_eq!(parse_limit("500"), Ok(500));
    }

    #[test]
    fn search_queries_are_bounded_before_cache_access() {
        assert_eq!(parse_search_query("  rust  "), Ok("rust".to_owned()));
        assert!(parse_search_query("\n").is_err());
        assert!(parse_search_query(&"x".repeat(4_001)).is_err());
        assert!(Cli::try_parse_from(["hnx", "search", &"x".repeat(4_001)]).is_err());
    }

    #[test]
    fn page_context_and_request_ids_reject_superseded_results() {
        let page = StoryPage {
            feed: Feed::Top,
            query: None,
            items: Vec::new(),
            source: Source::Cache,
            stale: false,
            fetched_at: 1,
        };
        let mut app = App::new(page.clone());
        assert!(PageContext::Feed(Feed::Top).matches(&app));
        assert!(!PageContext::Search("rust".to_owned()).matches(&app));

        app.load_page(StoryPage {
            query: Some("rust".to_owned()),
            ..page
        });
        assert!(PageContext::Search("rust".to_owned()).matches(&app));
        assert!(!PageContext::Feed(Feed::Top).matches(&app));
        assert_eq!(next_request_id(u64::MAX), 1);
    }

    #[test]
    fn offline_open_is_rejected_before_os_handoff() {
        let mut app = App::new(StoryPage {
            feed: Feed::Top,
            query: None,
            items: vec![Item {
                id: 7,
                url: Some("https://example.com".to_owned()),
                ..Item::default()
            }],
            source: Source::Cache,
            stale: false,
            fetched_at: 1,
        });
        app.set_offline(true);

        handle_open_story(&mut app, 7);

        assert_eq!(
            app.error(),
            Some("Opening URLs is unavailable in offline mode")
        );
    }

    #[test]
    fn json_output_is_one_record_terminated_by_a_newline() {
        let value = serde_json::json!({"title": "hello", "rank": 1});
        let mut output = Vec::new();
        write_buffered(&mut output, |writer| write_json(writer, &value))
            .expect("JSON output succeeds");

        assert_eq!(output.last(), Some(&b'\n'));
        assert_eq!(
            std::str::from_utf8(&output)
                .expect("JSON output is UTF-8")
                .lines()
                .count(),
            1
        );
        let decoded: serde_json::Value =
            serde_json::from_slice(&output[..output.len() - 1]).expect("record is valid JSON");
        assert_eq!(decoded, value);
    }

    #[test]
    fn broken_pipe_is_success_for_buffered_and_json_writes() {
        let buffered = write_buffered(
            ErrorWriter {
                kind: ErrorKind::BrokenPipe,
            },
            |writer| {
                writer.write_all(b"small buffered record\n")?;
                Ok(())
            },
        );
        assert!(buffered.is_ok());

        let large_value = "x".repeat(32 * 1024);
        let json = write_buffered(
            ErrorWriter {
                kind: ErrorKind::BrokenPipe,
            },
            |writer| write_json(writer, &large_value),
        );
        assert!(json.is_ok());

        let other_error = write_buffered(
            ErrorWriter {
                kind: ErrorKind::PermissionDenied,
            },
            |writer| {
                writer.write_all(b"record\n")?;
                Ok(())
            },
        );
        assert!(
            matches!(other_error, Err(CliError::Io(error)) if error.kind() == ErrorKind::PermissionDenied)
        );
    }

    #[test]
    fn deeply_nested_comments_are_written_iteratively_in_preorder() {
        const DEPTH: u32 = 10_000;
        let mut nested = None;
        for depth in (0..DEPTH).rev() {
            let children = nested.take().map_or_else(Vec::new, |child| vec![child]);
            nested = Some(Comment {
                id: u64::from(depth),
                depth,
                children,
                ..Comment::default()
            });
        }
        let root = ManuallyDrop::new(nested.expect("deep tree has a root"));
        let mut output = Vec::new();

        write_comments(&mut output, std::slice::from_ref(&*root))
            .expect("deep tree output succeeds without recursive calls");

        assert_eq!(
            std::str::from_utf8(&output)
                .expect("comment output is UTF-8")
                .lines()
                .count(),
            usize::try_from(DEPTH).expect("test depth fits usize")
        );
        assert!(output.starts_with(b"unknown [id:0]\n"));
        assert!(output.ends_with(b"unknown [id:9999]\n"));
    }

    #[test]
    fn stale_text_markers_are_first_and_partial_threads_disclose_completeness() {
        let item = Item {
            id: 7,
            title: Some("Example".to_owned()),
            kids: vec![11, 12],
            descendants: 2,
            ..Item::default()
        };
        let page = StoryPage {
            feed: Feed::Top,
            query: None,
            items: vec![item.clone()],
            source: Source::Cache,
            stale: true,
            fetched_at: 123,
        };
        let thread = Thread {
            item: item.clone(),
            comments: vec![Comment {
                id: 11,
                ..Comment::default()
            }],
            source: Source::Cache,
            stale: true,
            fetched_at: 123,
        };

        let mut page_output = Vec::new();
        write_page(&mut page_output, &page, OutputFormat::Text).expect("page writes");
        assert!(page_output.starts_with(b"[stale cache \xc2\xb7 fetched_at 123]\n"));

        let mut item_output = Vec::new();
        write_item(
            &mut item_output,
            &item,
            Source::Cache,
            true,
            123,
            OutputFormat::Text,
        )
        .expect("item writes");
        assert!(item_output.starts_with(b"[stale cache \xc2\xb7 fetched_at 123]\n"));

        let mut thread_output = Vec::new();
        write_thread(&mut thread_output, &thread, OutputFormat::Text).expect("thread writes");
        let thread_output = String::from_utf8(thread_output).expect("thread output is UTF-8");
        let mut lines = thread_output.lines();
        assert_eq!(lines.next(), Some("[stale cache · fetched_at 123]"));
        assert_eq!(
            lines.next(),
            Some("[partial thread · loaded 1 of 2 comments · omitted 1 · unresolved 1]")
        );

        let mut json_output = Vec::new();
        write_thread(&mut json_output, &thread, OutputFormat::Json).expect("thread JSON writes");
        let json: serde_json::Value =
            serde_json::from_slice(&json_output).expect("thread JSON is valid");
        assert_eq!(json["data"]["metadata"]["partial"], true);
        assert_eq!(json["data"]["metadata"]["omitted_comments"], 1);
        assert!(json["data"].get("source").is_none());
    }

    #[test]
    fn cleanup_guards_cover_partial_failure_and_cleanup_is_idempotent() {
        let guard_calls = Cell::new(0_u8);
        {
            let _guard = CleanupGuard::new(|| guard_calls.set(guard_calls.get() + 1));
        }
        CleanupGuard::new(|| guard_calls.set(guard_calls.get() + 1)).disarm();
        assert_eq!(guard_calls.get(), 1);

        let active = AtomicBool::new(true);
        let cleanup_calls = Cell::new(0_u8);
        cleanup_once(&active, || cleanup_calls.set(cleanup_calls.get() + 1));
        cleanup_once(&active, || cleanup_calls.set(cleanup_calls.get() + 1));
        assert_eq!(cleanup_calls.get(), 1);
    }
}
