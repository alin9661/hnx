//! Event-driven application state for the terminal interface.

use std::collections::BTreeSet;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use regex::RegexBuilder;

use crate::model::{Comment, Feed, Item, Source, StoryPage, Thread};

/// The content pane that receives navigation input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FocusPane {
    #[default]
    Stories,
    Thread,
    Detail,
}

impl FocusPane {
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Stories => Self::Thread,
            Self::Thread => Self::Detail,
            Self::Detail => Self::Stories,
        }
    }

    #[must_use]
    pub const fn previous(self) -> Self {
        match self {
            Self::Stories => Self::Detail,
            Self::Thread => Self::Stories,
            Self::Detail => Self::Thread,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Stories => "stories",
            Self::Thread => "thread",
            Self::Detail => "detail",
        }
    }
}

/// Which secondary pane remains visible beside stories in the two-pane layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SecondaryPane {
    #[default]
    Thread,
    Detail,
}

/// The active text prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    Search,
    Filter,
}

impl PromptKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Search => "Search HN",
            Self::Filter => "Filter stories",
        }
    }
}

/// Editable prompt state. The cursor is always at the end of the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub kind: PromptKind,
    pub value: String,
}

/// Render-ready article content, independent of the fetch/extraction implementation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArticleView {
    pub title: String,
    pub url: Option<String>,
    pub body: String,
}

impl ArticleView {
    #[must_use]
    pub fn new(title: impl Into<String>, url: Option<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            url,
            body: body.into(),
        }
    }
}

/// A side effect requested by a user event. The caller performs I/O and loads the result back
/// into [`App`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AppAction {
    #[default]
    None,
    Quit,
    LoadFeed(Feed),
    LoadThread(u64),
    Search(String),
    Refresh,
    OpenStory(u64),
    LoadArticle(u64),
    SetOffline(bool),
    BookmarkChanged {
        item_id: u64,
        bookmarked: bool,
    },
}

impl AppAction {
    #[must_use]
    pub const fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Selection {
    index: Option<usize>,
    offset: usize,
    viewport: usize,
}

impl Selection {
    const fn empty() -> Self {
        Self {
            index: None,
            offset: 0,
            viewport: 1,
        }
    }

    fn reset(&mut self, len: usize) {
        self.index = (len > 0).then_some(0);
        self.offset = 0;
        self.clamp(len);
    }

    fn set_viewport(&mut self, viewport: usize, len: usize) {
        self.viewport = viewport.max(1);
        self.clamp(len);
    }

    fn clamp(&mut self, len: usize) {
        if len == 0 {
            self.index = None;
            self.offset = 0;
            return;
        }

        let index = self.index.unwrap_or(0).min(len - 1);
        self.index = Some(index);
        let max_offset = len.saturating_sub(self.viewport);
        self.offset = self.offset.min(max_offset);
        self.reveal(index);
    }

    fn reveal(&mut self, index: usize) {
        if index < self.offset {
            self.offset = index;
        } else if index >= self.offset.saturating_add(self.viewport) {
            self.offset = index.saturating_add(1).saturating_sub(self.viewport);
        }
    }

    fn move_by(&mut self, delta: isize, len: usize) {
        if len == 0 {
            self.reset(0);
            return;
        }
        let current = self.index.unwrap_or(0);
        let next = current.saturating_add_signed(delta).min(len - 1);
        self.index = Some(next);
        self.reveal(next);
    }

    fn page_by(&mut self, pages: isize, len: usize) {
        let amount = isize::try_from(self.viewport.max(1)).unwrap_or(isize::MAX);
        self.move_by(pages.saturating_mul(amount), len);
    }

    fn half_page_by(&mut self, pages: isize, len: usize) {
        let half_viewport = self.viewport.div_ceil(2).max(1);
        let amount = isize::try_from(half_viewport).unwrap_or(isize::MAX);
        self.move_by(pages.saturating_mul(amount), len);
    }

    fn first(&mut self, len: usize) {
        if len > 0 {
            self.index = Some(0);
            self.offset = 0;
        }
    }

    fn last(&mut self, len: usize) {
        if len > 0 {
            let index = len - 1;
            self.index = Some(index);
            self.reveal(index);
        }
    }
}

/// All mutable state needed to render and navigate hnx.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct App {
    feed: Feed,
    stories: Vec<Item>,
    visible_story_indices: Vec<usize>,
    thread: Option<Thread>,
    flat_comments: Vec<Comment>,
    visible_comment_indices: Vec<usize>,
    collapsed_comments: BTreeSet<u64>,
    thread_partial: bool,
    article: Option<ArticleView>,
    page_source: Option<Source>,
    page_stale: bool,
    page_fetched_at: Option<i64>,
    source: Option<Source>,
    stale: bool,
    fetched_at: Option<i64>,
    offline: bool,
    loading: bool,
    status: Option<String>,
    error: Option<String>,
    focus: FocusPane,
    secondary: SecondaryPane,
    story_selection: Selection,
    comment_selection: Selection,
    detail_scroll: u16,
    detail_viewport: usize,
    filter: String,
    filter_regex: Option<regex::Regex>,
    search_query: Option<String>,
    bookmarks: BTreeSet<u64>,
    bookmarks_only: bool,
    prompt: Option<Prompt>,
    help_visible: bool,
}

impl App {
    /// Creates application state from an already loaded page.
    #[must_use]
    pub fn new(page: StoryPage) -> Self {
        let mut app = Self::empty(page.feed);
        app.set_page(page);
        app
    }

    /// Creates application state before the first page has loaded.
    #[must_use]
    pub fn empty(feed: Feed) -> Self {
        Self {
            feed,
            stories: Vec::new(),
            visible_story_indices: Vec::new(),
            thread: None,
            flat_comments: Vec::new(),
            visible_comment_indices: Vec::new(),
            collapsed_comments: BTreeSet::new(),
            thread_partial: false,
            article: None,
            page_source: None,
            page_stale: false,
            page_fetched_at: None,
            source: None,
            stale: false,
            fetched_at: None,
            offline: false,
            loading: false,
            status: None,
            error: None,
            focus: FocusPane::Stories,
            secondary: SecondaryPane::Thread,
            story_selection: Selection::empty(),
            comment_selection: Selection::empty(),
            detail_scroll: 0,
            detail_viewport: 1,
            filter: String::new(),
            filter_regex: None,
            search_query: None,
            bookmarks: BTreeSet::new(),
            bookmarks_only: false,
            prompt: None,
            help_visible: false,
        }
    }

    #[must_use]
    pub const fn feed(&self) -> Feed {
        self.feed
    }

    #[must_use]
    pub const fn source(&self) -> Option<Source> {
        self.source
    }

    #[must_use]
    pub const fn stale(&self) -> bool {
        self.stale
    }

    #[must_use]
    pub const fn fetched_at(&self) -> Option<i64> {
        self.fetched_at
    }

    #[must_use]
    pub const fn offline(&self) -> bool {
        self.offline
    }

    #[must_use]
    pub const fn loading(&self) -> bool {
        self.loading
    }

    /// Whether the loaded comment tree is known to omit comments.
    #[must_use]
    pub const fn thread_partial(&self) -> bool {
        self.thread_partial
    }

    #[must_use]
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    #[must_use]
    pub const fn focus(&self) -> FocusPane {
        self.focus
    }

    #[must_use]
    pub const fn secondary_pane(&self) -> SecondaryPane {
        self.secondary
    }

    #[must_use]
    pub const fn help_visible(&self) -> bool {
        self.help_visible
    }

    #[must_use]
    pub fn prompt(&self) -> Option<&Prompt> {
        self.prompt.as_ref()
    }

    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    #[must_use]
    pub fn search_query(&self) -> Option<&str> {
        self.search_query.as_deref()
    }

    #[must_use]
    pub const fn bookmarks_only(&self) -> bool {
        self.bookmarks_only
    }

    #[must_use]
    pub fn bookmarks(&self) -> &BTreeSet<u64> {
        &self.bookmarks
    }

    #[must_use]
    pub fn is_bookmarked(&self, item_id: u64) -> bool {
        self.bookmarks.contains(&item_id)
    }

    #[must_use]
    pub fn stories(&self) -> &[Item] {
        &self.stories
    }

    /// Iterates over stories after applying the local filter and bookmarks-only view.
    pub fn visible_items(&self) -> impl Iterator<Item = &Item> {
        self.visible_story_indices
            .iter()
            .filter_map(|index| self.stories.get(*index))
    }

    /// Iterates over one already-indexed viewport of visible stories.
    pub fn visible_item_window(&self, offset: usize, limit: usize) -> impl Iterator<Item = &Item> {
        let end = offset
            .saturating_add(limit)
            .min(self.visible_story_indices.len());
        self.visible_story_indices
            .get(offset..end)
            .unwrap_or_default()
            .iter()
            .filter_map(|index| self.stories.get(*index))
    }

    #[must_use]
    pub fn visible_item_count(&self) -> usize {
        self.visible_story_indices.len()
    }

    #[must_use]
    pub fn selected_item(&self) -> Option<&Item> {
        let visible_index = self.story_selection.index?;
        let story_index = *self.visible_story_indices.get(visible_index)?;
        self.stories.get(story_index)
    }

    #[must_use]
    pub const fn selected_story_index(&self) -> Option<usize> {
        self.story_selection.index
    }

    #[must_use]
    pub const fn story_offset(&self) -> usize {
        self.story_selection.offset
    }

    #[must_use]
    pub fn thread(&self) -> Option<&Thread> {
        self.thread.as_ref()
    }

    #[must_use]
    pub fn selected_comment(&self) -> Option<&Comment> {
        let visible_index = self.comment_selection.index?;
        let flat_index = *self.visible_comment_indices.get(visible_index)?;
        self.flat_comments.get(flat_index)
    }

    #[must_use]
    pub const fn selected_comment_index(&self) -> Option<usize> {
        self.comment_selection.index
    }

    #[must_use]
    pub const fn comment_offset(&self) -> usize {
        self.comment_selection.offset
    }

    /// Returns every comment in pre-order. The flattened view is built once when a thread loads,
    /// allowing render work to remain proportional to the viewport.
    #[must_use]
    pub fn flattened_comments(&self) -> &[Comment] {
        &self.flat_comments
    }

    /// Iterates over comments after omitting descendants of collapsed nodes.
    pub fn visible_comments(&self) -> impl Iterator<Item = &Comment> {
        self.visible_comment_indices
            .iter()
            .filter_map(|index| self.flat_comments.get(*index))
    }

    /// Iterates over one already-indexed viewport of visible comments.
    pub fn visible_comment_window(
        &self,
        offset: usize,
        limit: usize,
    ) -> impl Iterator<Item = &Comment> {
        let end = offset
            .saturating_add(limit)
            .min(self.visible_comment_indices.len());
        self.visible_comment_indices
            .get(offset..end)
            .unwrap_or_default()
            .iter()
            .filter_map(|index| self.flat_comments.get(*index))
    }

    #[must_use]
    pub fn collapsed_comment_ids(&self) -> &BTreeSet<u64> {
        &self.collapsed_comments
    }

    #[must_use]
    pub fn is_comment_collapsed(&self, comment_id: u64) -> bool {
        self.collapsed_comments.contains(&comment_id)
    }

    #[must_use]
    pub fn comment_count(&self) -> usize {
        self.visible_comment_indices.len()
    }

    #[must_use]
    pub fn article(&self) -> Option<&ArticleView> {
        self.article.as_ref()
    }

    #[must_use]
    pub const fn detail_scroll(&self) -> u16 {
        self.detail_scroll
    }

    /// Replaces the story page while retaining local preferences such as bookmarks and offline
    /// mode.
    pub fn set_page(&mut self, page: StoryPage) {
        self.feed = page.feed;
        self.search_query = page.query;
        self.stories = page.items;
        self.page_source = Some(page.source);
        self.page_stale = page.stale;
        self.page_fetched_at = Some(page.fetched_at);
        self.restore_page_metadata();
        self.thread = None;
        self.flat_comments.clear();
        self.visible_comment_indices.clear();
        self.collapsed_comments.clear();
        self.thread_partial = false;
        self.article = None;
        self.detail_scroll = 0;
        self.loading = false;
        self.error = None;
        self.status = Some(format!("{} stories loaded", self.stories.len()));
        self.rebuild_visible_stories();
        self.reset_story_selection();
        self.comment_selection.reset(0);
        self.focus = FocusPane::Stories;
        self.secondary = SecondaryPane::Thread;
    }

    /// Alias for callers that describe page replacement as loading.
    pub fn load_page(&mut self, page: StoryPage) {
        self.set_page(page);
    }

    /// Replaces data for the active feed/search without discarding reading context.
    ///
    /// Story identity is preserved by item id rather than rank because a refreshed
    /// Hacker News feed can reorder while the user is reading it.
    pub fn refresh_page(&mut self, page: StoryPage) {
        let same_context = self.feed == page.feed && self.search_query == page.query;
        if !same_context {
            self.set_page(page);
            return;
        }

        let selected_id = self.selected_item().map(|item| item.id);
        self.feed = page.feed;
        self.search_query = page.query;
        self.stories = page.items;
        self.page_source = Some(page.source);
        self.page_stale = page.stale;
        self.page_fetched_at = Some(page.fetched_at);
        self.loading = false;
        self.error = None;
        self.rebuild_visible_stories();

        let selected_index = selected_id.and_then(|item_id| {
            self.visible_story_indices
                .iter()
                .position(|story_index| self.stories[*story_index].id == item_id)
        });
        if let Some(index) = selected_index {
            self.story_selection.index = Some(index);
            self.story_selection.clamp(self.visible_story_indices.len());
            if self.thread.is_none() && self.article.is_none() {
                self.restore_page_metadata();
            }
        } else {
            self.story_selection.reset(self.visible_story_indices.len());
            self.clear_story_context();
            self.set_focus(FocusPane::Stories);
        }
        self.status = Some(format!("{} stories refreshed", self.stories.len()));
    }

    pub fn load_thread(&mut self, thread: Thread) {
        let thread_metadata = thread.metadata();
        self.flat_comments = flatten_comments(&thread.comments);
        self.collapsed_comments.clear();
        self.rebuild_visible_comments();
        let comment_count = self.comment_count();
        self.source = Some(thread.source);
        self.stale = thread.stale;
        self.fetched_at = Some(thread.fetched_at);
        self.thread_partial = thread_metadata.partial;
        self.thread = Some(thread);
        self.article = None;
        self.detail_scroll = 0;
        self.comment_selection.reset(comment_count);
        self.loading = false;
        self.error = None;
        self.status = Some(if thread_metadata.partial {
            format!(
                "{comment_count} comments loaded · {} omitted",
                thread_metadata.omitted_comments
            )
        } else {
            format!("{comment_count} comments loaded")
        });
        self.set_focus(FocusPane::Thread);
    }

    pub fn set_article(&mut self, article: ArticleView) {
        self.article = Some(article);
        self.detail_scroll = 0;
        self.loading = false;
        self.error = None;
        self.set_focus(FocusPane::Detail);
    }

    pub fn clear_article(&mut self) {
        self.article = None;
        self.detail_scroll = 0;
    }

    pub fn set_bookmarks<I>(&mut self, bookmarks: I)
    where
        I: IntoIterator<Item = u64>,
    {
        self.bookmarks = bookmarks.into_iter().collect();
        self.rebuild_visible_stories();
        self.reset_story_selection_after_view_change();
    }

    pub fn set_bookmarked(&mut self, item_id: u64, bookmarked: bool) {
        if bookmarked {
            self.bookmarks.insert(item_id);
        } else {
            self.bookmarks.remove(&item_id);
        }
        if self.bookmarks_only {
            self.rebuild_visible_stories();
            self.reset_story_selection_after_view_change();
        }
    }

    pub fn set_offline(&mut self, offline: bool) {
        self.offline = offline;
        self.status = Some(if offline {
            "Offline mode enabled".to_owned()
        } else {
            "Online mode enabled".to_owned()
        });
    }

    pub fn set_loading(&mut self, loading: bool) {
        self.loading = loading;
        if loading {
            self.error = None;
        }
    }

    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = Some(status.into());
        self.error = None;
    }

    pub fn clear_status(&mut self) {
        self.status = None;
    }

    pub fn set_error(&mut self, error: impl Into<String>) {
        self.error = Some(error.into());
        self.loading = false;
    }

    pub fn clear_error(&mut self) {
        self.error = None;
    }

    pub fn set_focus(&mut self, focus: FocusPane) {
        self.focus = focus;
        match focus {
            FocusPane::Thread => self.secondary = SecondaryPane::Thread,
            FocusPane::Detail => self.secondary = SecondaryPane::Detail,
            FocusPane::Stories => {}
        }
    }

    /// Records the number of logical rows visible in each selectable pane. The renderer calls
    /// this after computing the responsive layout; no timer or tick is involved.
    pub fn set_viewports(&mut self, story_rows: usize, comment_rows: usize) {
        let story_count = self.visible_item_count();
        let comment_count = self.comment_count();
        self.story_selection.set_viewport(story_rows, story_count);
        self.comment_selection
            .set_viewport(comment_rows, comment_count);
    }

    /// Records the number of wrapped article rows visible in the Detail pane.
    pub(crate) fn set_detail_viewport(&mut self, rows: usize) {
        self.detail_viewport = rows.max(1);
    }

    /// Maps a terminal key event into a state transition and optional I/O action.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn handle_key(&mut self, key: KeyEvent) -> AppAction {
        if key.kind == KeyEventKind::Release {
            return AppAction::None;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'q'))
        {
            return AppAction::Quit;
        }

        if self.prompt.is_some() {
            return self.handle_prompt_key(key);
        }

        if self.help_visible {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                self.help_visible = false;
            }
            return AppAction::None;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('d') => {
                    self.half_page_selection(1);
                    return AppAction::None;
                }
                KeyCode::Char('u') => {
                    self.half_page_selection(-1);
                    return AppAction::None;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Char('q') => AppAction::Quit,
            KeyCode::Char('?') => {
                self.help_visible = true;
                AppAction::None
            }
            KeyCode::Char('/') => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::Search,
                    value: self.search_query.clone().unwrap_or_default(),
                });
                AppAction::None
            }
            KeyCode::Char('f') => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::Filter,
                    value: self.filter.clone(),
                });
                AppAction::None
            }
            KeyCode::Char('b' | ' ') => self.toggle_selected_bookmark(),
            KeyCode::Char('B') => {
                self.bookmarks_only = !self.bookmarks_only;
                self.rebuild_visible_stories();
                self.reset_story_selection_after_view_change();
                AppAction::None
            }
            KeyCode::Char('O') => {
                self.set_offline(!self.offline);
                AppAction::SetOffline(self.offline)
            }
            KeyCode::Char('r') => {
                self.loading = true;
                AppAction::Refresh
            }
            KeyCode::Char('t') => {
                self.set_focus(FocusPane::Thread);
                AppAction::None
            }
            KeyCode::Char('d') => {
                self.set_focus(FocusPane::Detail);
                AppAction::None
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_selection(1);
                AppAction::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_selection(-1);
                AppAction::None
            }
            KeyCode::PageDown => {
                self.page_selection(1);
                AppAction::None
            }
            KeyCode::PageUp => {
                self.page_selection(-1);
                AppAction::None
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.select_first();
                AppAction::None
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.select_last();
                AppAction::None
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.toggle_panel_focus();
                AppAction::None
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.focus_secondary_panel();
                AppAction::None
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.set_focus(FocusPane::Stories);
                AppAction::None
            }
            KeyCode::Char('[') => self.change_feed(-1),
            KeyCode::Char(']') => self.change_feed(1),
            KeyCode::Char(character @ '1'..='6') => {
                let index = character
                    .to_digit(10)
                    .and_then(|digit| usize::try_from(digit.saturating_sub(1)).ok())
                    .unwrap_or_default();
                self.choose_feed(Feed::ALL[index])
            }
            KeyCode::Enter => self.activate_selected(),
            KeyCode::Char('a') => self.load_selected_article(),
            KeyCode::Char('o' | 'v') => self.open_selected(),
            KeyCode::Esc => {
                self.error = None;
                self.status = None;
                self.set_focus(FocusPane::Stories);
                AppAction::None
            }
            _ => AppAction::None,
        }
    }

    fn story_is_visible(&self, item: &Item) -> bool {
        if self.bookmarks_only && !self.bookmarks.contains(&item.id) {
            return false;
        }
        let Some(regex) = &self.filter_regex else {
            return true;
        };
        item.title
            .as_deref()
            .is_some_and(|title| regex.is_match(title))
            || item
                .by
                .as_deref()
                .is_some_and(|author| regex.is_match(author))
            || item.url.as_deref().is_some_and(|url| regex.is_match(url))
    }

    fn reset_story_selection(&mut self) {
        let len = self.visible_item_count();
        self.story_selection.reset(len);
    }

    fn reset_story_selection_after_view_change(&mut self) {
        let selected_id = self.selected_item().map(|item| item.id);
        self.reset_story_selection();
        if self.selected_item().map(|item| item.id) != selected_id {
            self.clear_story_context();
        }
    }

    fn clear_story_context(&mut self) {
        self.thread = None;
        self.flat_comments.clear();
        self.visible_comment_indices.clear();
        self.collapsed_comments.clear();
        self.thread_partial = false;
        self.article = None;
        self.comment_selection.reset(0);
        self.detail_scroll = 0;
        self.restore_page_metadata();
    }

    fn restore_page_metadata(&mut self) {
        self.source = self.page_source;
        self.stale = self.page_stale;
        self.fetched_at = self.page_fetched_at;
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => {
                self.prompt = None;
                AppAction::None
            }
            KeyCode::Enter => {
                let Some(prompt) = self.prompt.clone() else {
                    return AppAction::None;
                };
                match prompt.kind {
                    PromptKind::Search => {
                        self.prompt = None;
                        let query = prompt.value.trim().to_owned();
                        if !query.is_empty() {
                            self.begin_page_load(
                                self.feed,
                                Some(query.clone()),
                                format!("Searching for {query}"),
                            );
                        }
                        AppAction::Search(query)
                    }
                    PromptKind::Filter => {
                        let pattern = prompt.value.trim();
                        if pattern.is_empty() {
                            self.prompt = None;
                            self.filter.clear();
                            self.filter_regex = None;
                            self.error = None;
                            self.rebuild_visible_stories();
                            self.reset_story_selection_after_view_change();
                        } else {
                            match RegexBuilder::new(pattern).case_insensitive(true).build() {
                                Ok(regex) => {
                                    self.prompt = None;
                                    pattern.clone_into(&mut self.filter);
                                    self.filter_regex = Some(regex);
                                    self.error = None;
                                    self.rebuild_visible_stories();
                                    self.reset_story_selection_after_view_change();
                                }
                                Err(error) => {
                                    self.error = Some(format!("Invalid filter: {error}"));
                                }
                            }
                        }
                        AppAction::None
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(prompt) = &mut self.prompt {
                    prompt.value.pop();
                }
                AppAction::None
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some(prompt) = &mut self.prompt {
                    prompt.value.push(character);
                }
                AppAction::None
            }
            _ => AppAction::None,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        match self.focus {
            FocusPane::Stories => {
                let selected_id = self.selected_item().map(|item| item.id);
                let len = self.visible_item_count();
                self.story_selection.move_by(delta, len);
                if self.selected_item().map(|item| item.id) != selected_id {
                    self.clear_story_context();
                }
            }
            FocusPane::Thread => {
                let len = self.comment_count();
                self.comment_selection.move_by(delta, len);
            }
            FocusPane::Detail => {
                self.detail_scroll = self.detail_scroll.saturating_add_signed(
                    i16::try_from(delta.saturating_mul(3)).unwrap_or(if delta.is_negative() {
                        i16::MIN
                    } else {
                        i16::MAX
                    }),
                );
            }
        }
    }

    fn page_selection(&mut self, pages: isize) {
        match self.focus {
            FocusPane::Stories => {
                let selected_id = self.selected_item().map(|item| item.id);
                let len = self.visible_item_count();
                self.story_selection.page_by(pages, len);
                if self.selected_item().map(|item| item.id) != selected_id {
                    self.clear_story_context();
                }
            }
            FocusPane::Thread => {
                let len = self.comment_count();
                self.comment_selection.page_by(pages, len);
            }
            FocusPane::Detail => {
                self.scroll_detail_by(pages, self.detail_viewport);
            }
        }
    }

    fn half_page_selection(&mut self, pages: isize) {
        match self.focus {
            FocusPane::Stories => {
                let selected_id = self.selected_item().map(|item| item.id);
                let len = self.visible_item_count();
                self.story_selection.half_page_by(pages, len);
                if self.selected_item().map(|item| item.id) != selected_id {
                    self.clear_story_context();
                }
            }
            FocusPane::Thread => {
                let len = self.comment_count();
                self.comment_selection.half_page_by(pages, len);
            }
            FocusPane::Detail => {
                self.scroll_detail_by(pages, self.detail_viewport.div_ceil(2).max(1));
            }
        }
    }

    fn scroll_detail_by(&mut self, pages: isize, rows: usize) {
        let distance = pages.unsigned_abs().saturating_mul(rows);
        let distance = u16::try_from(distance).unwrap_or(u16::MAX);
        self.detail_scroll = if pages.is_negative() {
            self.detail_scroll.saturating_sub(distance)
        } else {
            self.detail_scroll.saturating_add(distance)
        };
    }

    fn focus_secondary_panel(&mut self) {
        self.set_focus(match self.secondary {
            SecondaryPane::Thread => FocusPane::Thread,
            SecondaryPane::Detail => FocusPane::Detail,
        });
    }

    fn toggle_panel_focus(&mut self) {
        if self.focus == FocusPane::Stories {
            self.focus_secondary_panel();
        } else {
            self.set_focus(FocusPane::Stories);
        }
    }

    fn select_first(&mut self) {
        match self.focus {
            FocusPane::Stories => {
                let selected_id = self.selected_item().map(|item| item.id);
                self.story_selection.first(self.visible_item_count());
                if self.selected_item().map(|item| item.id) != selected_id {
                    self.clear_story_context();
                }
            }
            FocusPane::Thread => self.comment_selection.first(self.comment_count()),
            FocusPane::Detail => self.detail_scroll = 0,
        }
    }

    fn select_last(&mut self) {
        match self.focus {
            FocusPane::Stories => {
                let selected_id = self.selected_item().map(|item| item.id);
                self.story_selection.last(self.visible_item_count());
                if self.selected_item().map(|item| item.id) != selected_id {
                    self.clear_story_context();
                }
            }
            FocusPane::Thread => self.comment_selection.last(self.comment_count()),
            FocusPane::Detail => {}
        }
    }

    fn activate_selected(&mut self) -> AppAction {
        match self.focus {
            FocusPane::Stories => {
                let Some(item_id) = self.selected_item().map(|item| item.id) else {
                    return AppAction::None;
                };
                self.loading = true;
                self.set_focus(FocusPane::Thread);
                AppAction::LoadThread(item_id)
            }
            FocusPane::Thread => {
                self.toggle_selected_comment();
                AppAction::None
            }
            FocusPane::Detail => AppAction::None,
        }
    }

    fn open_selected(&self) -> AppAction {
        self.selected_item()
            .map_or(AppAction::None, |item| AppAction::OpenStory(item.id))
    }

    fn load_selected_article(&mut self) -> AppAction {
        let Some(item_id) = self.selected_item().map(|item| item.id) else {
            return AppAction::None;
        };
        self.loading = true;
        self.set_focus(FocusPane::Detail);
        AppAction::LoadArticle(item_id)
    }

    fn toggle_selected_bookmark(&mut self) -> AppAction {
        let Some(item_id) = self.selected_item().map(|item| item.id) else {
            return AppAction::None;
        };
        let bookmarked = !self.bookmarks.contains(&item_id);
        self.set_bookmarked(item_id, bookmarked);
        AppAction::BookmarkChanged {
            item_id,
            bookmarked,
        }
    }

    fn choose_feed(&mut self, feed: Feed) -> AppAction {
        self.begin_page_load(feed, None, format!("Loading {}", feed.label()));
        AppAction::LoadFeed(feed)
    }

    fn begin_page_load(&mut self, feed: Feed, query: Option<String>, status: String) {
        self.feed = feed;
        self.search_query = query;
        self.stories.clear();
        self.visible_story_indices.clear();
        self.story_selection.reset(0);
        self.page_source = None;
        self.page_stale = false;
        self.page_fetched_at = None;
        self.clear_story_context();
        self.set_focus(FocusPane::Stories);
        self.loading = true;
        self.error = None;
        self.status = Some(status);
    }

    fn change_feed(&mut self, delta: isize) -> AppAction {
        let current = Feed::ALL
            .iter()
            .position(|feed| *feed == self.feed)
            .unwrap_or_default();
        let len = Feed::ALL.len();
        let next = current
            .saturating_add_signed(delta)
            .checked_rem(len)
            .unwrap_or_default();
        let next = if delta.is_negative() && current == 0 {
            len - 1
        } else {
            next
        };
        self.choose_feed(Feed::ALL[next])
    }

    fn toggle_selected_comment(&mut self) {
        let Some(comment_id) = self.selected_comment().map(|comment| comment.id) else {
            return;
        };
        let collapsed = if self.collapsed_comments.remove(&comment_id) {
            false
        } else {
            self.collapsed_comments.insert(comment_id);
            true
        };
        self.rebuild_visible_comments();
        let comment_count = self.comment_count();
        self.comment_selection.clamp(comment_count);
        self.status = Some(if collapsed {
            "Comment collapsed".to_owned()
        } else {
            "Comment expanded".to_owned()
        });
    }

    fn rebuild_visible_comments(&mut self) {
        self.visible_comment_indices.clear();
        let mut hidden_below = None;
        for (index, comment) in self.flat_comments.iter().enumerate() {
            if let Some(depth) = hidden_below {
                if comment.depth > depth {
                    continue;
                }
                hidden_below = None;
            }
            self.visible_comment_indices.push(index);
            if self.collapsed_comments.contains(&comment.id) {
                hidden_below = Some(comment.depth);
            }
        }
    }

    fn rebuild_visible_stories(&mut self) {
        let indices = self
            .stories
            .iter()
            .enumerate()
            .filter_map(|(index, item)| self.story_is_visible(item).then_some(index))
            .collect();
        self.visible_story_indices = indices;
    }
}

fn flatten_comments(comments: &[Comment]) -> Vec<Comment> {
    let mut stack = Vec::with_capacity(comments.len());
    for comment in comments.iter().rev() {
        stack.push((comment, comment.depth));
    }
    let mut output = Vec::with_capacity(comments.len());
    while let Some((comment, depth)) = stack.pop() {
        output.push(Comment {
            id: comment.id,
            by: comment.by.clone(),
            text: comment.text.clone(),
            time: comment.time,
            parent: comment.parent,
            kids: if comment.kids.is_empty() {
                comment.children.iter().map(|child| child.id).collect()
            } else {
                comment.kids.clone()
            },
            children: Vec::new(),
            deleted: comment.deleted,
            dead: comment.dead,
            depth,
        });
        for child in comment.children.iter().rev() {
            stack.push((child, depth.saturating_add(1)));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{App, AppAction, FocusPane};
    use crate::model::{Comment, Feed, Item, Source, StoryPage, Thread};

    fn page() -> StoryPage {
        StoryPage {
            feed: Feed::Top,
            query: None,
            items: (1..=8)
                .map(|id| Item {
                    id,
                    by: Some(if id == 4 { "alice" } else { "bob" }.to_owned()),
                    title: Some(format!("Story {id}")),
                    url: Some(format!("https://example.com/{id}")),
                    ..Item::default()
                })
                .collect(),
            source: Source::Algolia,
            stale: false,
            fetched_at: 42,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn selection_and_scroll_follow_viewport() {
        let mut app = App::new(page());
        app.set_viewports(3, 3);

        for _ in 0..5 {
            assert_eq!(app.handle_key(key(KeyCode::Down)), AppAction::None);
        }

        assert_eq!(app.selected_item().map(|item| item.id), Some(6));
        assert_eq!(app.story_offset(), 3);
        assert_eq!(app.handle_key(key(KeyCode::PageUp)), AppAction::None);
        assert_eq!(app.selected_item().map(|item| item.id), Some(3));
        assert_eq!(app.story_offset(), 2);
    }

    #[test]
    fn vim_half_pages_and_physical_keys_page_the_story_viewport() {
        let mut app = App::new(page());
        app.set_viewports(5, 5);

        assert_eq!(
            app.handle_key(ctrl_key(KeyCode::Char('d'))),
            AppAction::None
        );
        assert_eq!(app.selected_item().map(|item| item.id), Some(4));
        assert_eq!(app.focus(), FocusPane::Stories);

        let _ = app.handle_key(key(KeyCode::PageDown));
        assert_eq!(app.selected_item().map(|item| item.id), Some(8));

        let _ = app.handle_key(ctrl_key(KeyCode::Char('u')));
        assert_eq!(app.selected_item().map(|item| item.id), Some(5));

        let _ = app.handle_key(key(KeyCode::PageUp));
        assert_eq!(app.selected_item().map(|item| item.id), Some(1));
    }

    #[test]
    fn paging_story_selection_clears_dependent_content() {
        let mut app = App::new(page());
        let selected = app.selected_item().cloned().expect("story is selected");
        app.load_thread(Thread {
            item: selected,
            comments: Vec::new(),
            source: Source::Firebase,
            stale: false,
            fetched_at: 43,
        });
        app.set_focus(FocusPane::Stories);
        app.set_viewports(5, 5);

        let _ = app.handle_key(ctrl_key(KeyCode::Char('d')));

        assert_eq!(app.selected_item().map(|item| item.id), Some(4));
        assert!(app.thread().is_none());
        assert_eq!(app.source(), Some(Source::Algolia));
    }

    #[test]
    fn paging_saturates_at_story_and_detail_boundaries() {
        let mut app = App::new(page());
        app.set_viewports(5, 5);

        let _ = app.handle_key(ctrl_key(KeyCode::Char('u')));
        let _ = app.handle_key(key(KeyCode::PageUp));
        assert_eq!(app.selected_item().map(|item| item.id), Some(1));

        let _ = app.handle_key(key(KeyCode::End));
        let _ = app.handle_key(ctrl_key(KeyCode::Char('d')));
        let _ = app.handle_key(key(KeyCode::PageDown));
        assert_eq!(app.selected_item().map(|item| item.id), Some(8));

        app.set_focus(FocusPane::Detail);
        app.set_detail_viewport(7);
        app.detail_scroll = u16::MAX - 2;
        let _ = app.handle_key(key(KeyCode::PageDown));
        assert_eq!(app.detail_scroll(), u16::MAX);
        let _ = app.handle_key(key(KeyCode::Home));
        let _ = app.handle_key(ctrl_key(KeyCode::Char('u')));
        assert_eq!(app.detail_scroll(), 0);
    }

    #[test]
    fn detail_paging_uses_its_actual_viewport() {
        let mut app = App::new(page());
        app.set_focus(FocusPane::Detail);
        app.set_detail_viewport(7);

        let _ = app.handle_key(key(KeyCode::PageDown));
        assert_eq!(app.detail_scroll(), 7);
        let _ = app.handle_key(ctrl_key(KeyCode::Char('d')));
        assert_eq!(app.detail_scroll(), 11);
        let _ = app.handle_key(ctrl_key(KeyCode::Char('u')));
        assert_eq!(app.detail_scroll(), 7);
        let _ = app.handle_key(key(KeyCode::PageUp));
        assert_eq!(app.detail_scroll(), 0);
    }

    #[test]
    fn plain_d_focuses_detail_while_ctrl_d_pages() {
        let mut app = App::new(page());
        app.set_viewports(5, 5);

        let _ = app.handle_key(ctrl_key(KeyCode::Char('d')));
        assert_eq!(app.focus(), FocusPane::Stories);
        assert_eq!(app.selected_item().map(|item| item.id), Some(4));

        let _ = app.handle_key(key(KeyCode::Char('d')));
        assert_eq!(app.focus(), FocusPane::Detail);
    }

    #[test]
    fn prompts_and_help_block_paging_keys() {
        let mut app = App::new(page());
        app.set_viewports(5, 5);

        let _ = app.handle_key(key(KeyCode::Char('/')));
        let _ = app.handle_key(ctrl_key(KeyCode::Char('d')));
        let _ = app.handle_key(key(KeyCode::PageDown));
        assert_eq!(app.selected_item().map(|item| item.id), Some(1));
        assert_eq!(app.prompt().map(|prompt| prompt.value.as_str()), Some(""));

        let _ = app.handle_key(key(KeyCode::Esc));
        let _ = app.handle_key(key(KeyCode::Char('?')));
        let _ = app.handle_key(ctrl_key(KeyCode::Char('d')));
        let _ = app.handle_key(key(KeyCode::PageDown));
        assert!(app.help_visible());
        assert_eq!(app.selected_item().map(|item| item.id), Some(1));
    }

    #[test]
    fn horizontal_keys_move_between_two_physical_panels() {
        let mut app = App::new(page());

        let _ = app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focus(), FocusPane::Thread);
        let _ = app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focus(), FocusPane::Stories);

        let _ = app.handle_key(key(KeyCode::Char('d')));
        assert_eq!(app.focus(), FocusPane::Detail);
        let _ = app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.focus(), FocusPane::Stories);
        let _ = app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.focus(), FocusPane::Detail);
        let _ = app.handle_key(key(KeyCode::BackTab));
        assert_eq!(app.focus(), FocusPane::Stories);

        let _ = app.handle_key(key(KeyCode::Char('t')));
        assert_eq!(app.focus(), FocusPane::Thread);
        assert_eq!(app.secondary_pane(), super::SecondaryPane::Thread);
    }

    #[test]
    fn thread_focus_uses_half_and_full_viewport_paging() {
        let mut app = App::new(page());
        app.load_thread(Thread {
            item: Item {
                id: 1,
                ..Item::default()
            },
            comments: (10..=17)
                .map(|id| Comment {
                    id,
                    ..Comment::default()
                })
                .collect(),
            source: Source::Firebase,
            stale: false,
            fetched_at: 43,
        });
        app.set_viewports(5, 5);

        let _ = app.handle_key(ctrl_key(KeyCode::Char('d')));
        assert_eq!(app.selected_comment().map(|comment| comment.id), Some(13));

        let _ = app.handle_key(key(KeyCode::PageDown));
        assert_eq!(app.selected_comment().map(|comment| comment.id), Some(17));

        let _ = app.handle_key(ctrl_key(KeyCode::Char('u')));
        assert_eq!(app.selected_comment().map(|comment| comment.id), Some(14));

        let _ = app.handle_key(key(KeyCode::PageUp));
        let _ = app.handle_key(key(KeyCode::PageUp));
        assert_eq!(app.selected_comment().map(|comment| comment.id), Some(10));
    }

    #[test]
    fn filter_and_bookmark_views_clamp_selection() {
        let mut app = App::new(page());
        app.set_bookmarks([2, 4]);
        assert_eq!(app.handle_key(key(KeyCode::Char('B'))), AppAction::None);
        assert_eq!(app.visible_item_count(), 2);

        let _ = app.handle_key(key(KeyCode::Char('f')));
        for character in "alice".chars() {
            let _ = app.handle_key(key(KeyCode::Char(character)));
        }
        let _ = app.handle_key(key(KeyCode::Enter));

        assert_eq!(app.visible_item_count(), 1);
        assert_eq!(app.selected_item().map(|item| item.id), Some(4));
    }

    #[test]
    fn keys_return_io_actions_without_doing_io() {
        let mut app = App::new(page());

        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            AppAction::LoadThread(1)
        );
        assert_eq!(app.focus(), FocusPane::Thread);

        app.set_focus(FocusPane::Stories);
        assert_eq!(
            app.handle_key(key(KeyCode::Char('b'))),
            AppAction::BookmarkChanged {
                item_id: 1,
                bookmarked: true,
            }
        );
        assert!(app.is_bookmarked(1));

        assert_eq!(
            app.handle_key(key(KeyCode::Char(']'))),
            AppAction::LoadFeed(Feed::New)
        );

        let mut loaded = page();
        loaded.feed = Feed::New;
        app.load_page(loaded);
        assert_eq!(
            app.handle_key(key(KeyCode::Char('a'))),
            AppAction::LoadArticle(1)
        );
        assert_eq!(app.focus(), FocusPane::Detail);
    }

    #[test]
    fn search_prompt_emits_search_action() {
        let mut app = App::new(page());
        let _ = app.handle_key(key(KeyCode::Char('/')));
        for character in "rust".chars() {
            let _ = app.handle_key(key(KeyCode::Char(character)));
        }

        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            AppAction::Search("rust".to_owned())
        );
        assert_eq!(app.search_query(), Some("rust"));
        assert!(app.loading());
    }

    #[test]
    fn invalid_regex_keeps_filter_prompt_open_with_feedback() {
        let mut app = App::new(page());
        let _ = app.handle_key(key(KeyCode::Char('f')));
        let _ = app.handle_key(key(KeyCode::Char('[')));
        let _ = app.handle_key(key(KeyCode::Enter));

        assert!(app.prompt().is_some());
        assert!(
            app.error()
                .is_some_and(|error| error.contains("Invalid filter"))
        );
        assert_eq!(app.visible_item_count(), 8);
    }

    #[test]
    fn thread_enter_collapses_and_expands_descendants() {
        let mut app = App::new(page());
        app.load_thread(Thread {
            item: Item {
                id: 1,
                ..Item::default()
            },
            comments: vec![Comment {
                id: 10,
                by: Some("parent".to_owned()),
                kids: vec![11],
                children: vec![Comment {
                    id: 11,
                    by: Some("child".to_owned()),
                    ..Comment::default()
                }],
                ..Comment::default()
            }],
            source: Source::Firebase,
            stale: false,
            fetched_at: 43,
        });
        assert_eq!(app.comment_count(), 2);

        let _ = app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.comment_count(), 1);
        assert!(app.is_comment_collapsed(10));

        let _ = app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.comment_count(), 2);
        assert!(!app.is_comment_collapsed(10));
    }

    #[test]
    fn partial_thread_state_survives_transient_status_changes() {
        let mut app = App::new(page());
        app.load_thread(Thread {
            item: Item {
                id: 1,
                descendants: 2,
                kids: vec![10, 11],
                ..Item::default()
            },
            comments: vec![Comment {
                id: 10,
                ..Comment::default()
            }],
            source: Source::Firebase,
            stale: false,
            fetched_at: 43,
        });

        assert!(app.thread_partial());
        let _ = app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.status(), Some("Comment collapsed"));
        assert!(app.thread_partial());

        app.set_focus(FocusPane::Stories);
        let _ = app.handle_key(key(KeyCode::Down));
        assert!(!app.thread_partial());
    }

    #[test]
    fn background_refresh_preserves_selected_story_and_thread_by_id() {
        let mut app = App::new(page());
        app.set_viewports(3, 3);
        let _ = app.handle_key(key(KeyCode::Down));
        let _ = app.handle_key(key(KeyCode::Down));
        let selected = app.selected_item().cloned().expect("story is selected");
        assert_eq!(selected.id, 3);

        app.load_thread(Thread {
            item: selected,
            comments: vec![Comment {
                id: 30,
                by: Some("reader".to_owned()),
                text: Some("complete comment body".to_owned()),
                ..Comment::default()
            }],
            source: Source::Firebase,
            stale: true,
            fetched_at: 43,
        });

        let mut refreshed = page();
        refreshed.items.reverse();
        refreshed.source = Source::Hybrid;
        refreshed.fetched_at = 99;
        app.refresh_page(refreshed);

        assert_eq!(app.selected_item().map(|item| item.id), Some(3));
        assert_eq!(app.thread().map(|thread| thread.item.id), Some(3));
        assert_eq!(app.focus(), FocusPane::Thread);
        assert_eq!(app.source(), Some(Source::Firebase));
        assert_eq!(app.fetched_at(), Some(43));
    }

    #[test]
    fn changing_story_selection_clears_dependent_content_and_restores_page_metadata() {
        let mut app = App::new(page());
        let selected = app.selected_item().cloned().expect("story is selected");
        app.load_thread(Thread {
            item: selected,
            comments: Vec::new(),
            source: Source::Firebase,
            stale: true,
            fetched_at: 43,
        });
        app.set_focus(FocusPane::Stories);

        let _ = app.handle_key(key(KeyCode::End));

        assert_eq!(app.selected_item().map(|item| item.id), Some(8));
        assert!(app.thread().is_none());
        assert_eq!(app.source(), Some(Source::Algolia));
        assert!(!app.stale());
        assert_eq!(app.fetched_at(), Some(42));
    }

    #[test]
    fn enter_in_detail_has_no_browser_side_effect() {
        let mut app = App::new(page());
        app.set_focus(FocusPane::Detail);

        assert_eq!(app.handle_key(key(KeyCode::Enter)), AppAction::None);
        assert_eq!(
            app.handle_key(key(KeyCode::Char('o'))),
            AppAction::OpenStory(1)
        );
    }

    #[test]
    fn changing_feed_or_search_context_never_relabels_old_stories() {
        let mut app = App::new(page());

        assert_eq!(
            app.handle_key(key(KeyCode::Char(']'))),
            AppAction::LoadFeed(Feed::New)
        );
        assert_eq!(app.feed(), Feed::New);
        assert!(app.stories().is_empty());
        assert!(app.source().is_none());

        app.load_page(page());
        let _ = app.handle_key(key(KeyCode::Char('/')));
        for character in "rust".chars() {
            let _ = app.handle_key(key(KeyCode::Char(character)));
        }
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            AppAction::Search("rust".to_owned())
        );
        assert_eq!(app.search_query(), Some("rust"));
        assert!(app.stories().is_empty());
        assert!(app.source().is_none());
    }
}
