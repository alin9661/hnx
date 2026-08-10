//! Responsive Ratatui rendering.

use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};

use crate::{
    app::{App, FocusPane, SecondaryPane},
    model::{Comment, Feed, Item},
    sanitize::{sanitize_single_line, sanitize_text},
    theme::Theme,
};

pub const WIDE_MIN_WIDTH: u16 = 120;
pub const MEDIUM_MIN_WIDTH: u16 = 80;

/// The responsive content arrangement selected for a terminal width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    /// Story list and the active secondary pane at the wide breakpoint.
    Wide,
    /// Story list and the active secondary pane.
    Medium,
    /// Only the focused pane.
    Narrow,
}

impl LayoutMode {
    #[must_use]
    pub const fn for_width(width: u16) -> Self {
        if width >= WIDE_MIN_WIDTH {
            Self::Wide
        } else if width >= MEDIUM_MIN_WIDTH {
            Self::Medium
        } else {
            Self::Narrow
        }
    }
}

/// Rectangles used by the renderer. `None` means that pane is hidden at this width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiLayout {
    pub mode: LayoutMode,
    pub tabs: Rect,
    pub stories: Option<Rect>,
    pub thread: Option<Rect>,
    pub detail: Option<Rect>,
    pub status: Rect,
}

/// Computes all UI regions without rendering or mutating application state.
#[must_use]
pub fn layout_for(area: Rect, focus: FocusPane, secondary: SecondaryPane) -> UiLayout {
    let mode = LayoutMode::for_width(area.width);
    let header_height = area.height.min(3);
    let remaining = area.height.saturating_sub(header_height);
    let status_height = remaining.min(2);
    let content_height = remaining.saturating_sub(status_height);

    let tabs = Rect::new(area.x, area.y, area.width, header_height);
    let content = Rect::new(
        area.x,
        area.y.saturating_add(header_height),
        area.width,
        content_height,
    );
    let status = Rect::new(
        area.x,
        area.y
            .saturating_add(header_height)
            .saturating_add(content_height),
        area.width,
        status_height,
    );

    let (stories, thread, detail) = match mode {
        LayoutMode::Wide | LayoutMode::Medium => {
            let [stories, secondary_area] = split_two(content, 44);
            match secondary {
                SecondaryPane::Thread => (Some(stories), Some(secondary_area), None),
                SecondaryPane::Detail => (Some(stories), None, Some(secondary_area)),
            }
        }
        LayoutMode::Narrow => match focus {
            FocusPane::Stories => (Some(content), None, None),
            FocusPane::Thread => (None, Some(content), None),
            FocusPane::Detail => (None, None, Some(content)),
        },
    };

    UiLayout {
        mode,
        tabs,
        stories,
        thread,
        detail,
        status,
    }
}

/// Draws the complete interface. Rendering has no clock/tick dependency; it only records the
/// current viewport capacities so subsequent navigation can keep selections visible.
pub fn render(frame: &mut Frame<'_>, app: &mut App, theme: &Theme) {
    let area = frame.area();
    if area.is_empty() {
        return;
    }

    frame.render_widget(
        Block::default().style(Style::default().bg(theme.background).fg(theme.foreground)),
        area,
    );

    let layout = layout_for(area, app.focus(), app.secondary_pane());
    let story_rows = layout.stories.map_or(1, |rect| {
        usize::from(rect.height.saturating_sub(2) / 2).max(1)
    });
    let comment_rows = layout.thread.map_or(1, |rect| {
        usize::from(rect.height.saturating_sub(2) / 2).max(1)
    });
    let detail_rows = layout
        .detail
        .map_or(1, |rect| usize::from(rect.height.saturating_sub(2)).max(1));
    app.set_viewports(story_rows, comment_rows);
    app.set_detail_viewport(detail_rows);

    render_tabs(frame, layout.tabs, app, theme);
    if let Some(rect) = layout.stories {
        render_stories(frame, rect, app, story_rows, theme);
    }
    if let Some(rect) = layout.thread {
        render_thread(frame, rect, app, comment_rows, theme);
    }
    if let Some(rect) = layout.detail {
        render_detail(frame, rect, app, theme);
    }
    render_status(frame, layout.status, app, layout.mode, theme);

    if app.help_visible() {
        render_help(frame, area, theme);
    } else if app.prompt().is_some() {
        render_prompt(frame, area, app, theme);
    }
}

fn split_two(area: Rect, left_percent: u16) -> [Rect; 2] {
    let left_width = area.width.saturating_mul(left_percent) / 100;
    [
        Rect::new(area.x, area.y, left_width, area.height),
        Rect::new(
            area.x.saturating_add(left_width),
            area.y,
            area.width.saturating_sub(left_width),
            area.height,
        ),
    ]
}

fn render_tabs(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let selected = Feed::ALL
        .iter()
        .position(|feed| *feed == app.feed())
        .unwrap_or_default();
    let titles = Feed::ALL.map(|feed| Line::from(feed.label()));
    let title = app.search_query().map_or_else(
        || " hnx ".to_owned(),
        |query| format!(" hnx · “{}” ", sanitize_single_line(query)),
    );
    let tabs = Tabs::new(titles)
        .select(selected)
        .divider(Span::styled(" │ ", Style::default().fg(theme.border)))
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border)),
        )
        .style(theme.muted_style())
        .highlight_style(theme.selected_style().add_modifier(Modifier::BOLD));
    frame.render_widget(tabs, area);
}

fn render_stories(frame: &mut Frame<'_>, area: Rect, app: &App, capacity: usize, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let title = if app.bookmarks_only() {
        format!(" Saved ({}) ", app.visible_item_count())
    } else if app.filter().is_empty() {
        format!(" Stories ({}) ", app.visible_item_count())
    } else {
        format!(
            " Stories ({}) · /{}/ ",
            app.visible_item_count(),
            sanitize_single_line(app.filter())
        )
    };
    let block = pane_block(title, app.focus() == FocusPane::Stories, theme);

    let offset = app.story_offset();
    let items: Vec<ListItem<'_>> = app
        .visible_item_window(offset, capacity)
        .map(|item| story_row(item, app.is_bookmarked(item.id), theme))
        .collect();

    if items.is_empty() {
        let message = if app.loading() {
            "Loading stories…"
        } else if app.bookmarks_only() {
            "No saved stories match this view."
        } else if app.filter().is_empty() {
            "No stories loaded."
        } else {
            "No stories match this filter."
        };
        frame.render_widget(
            Paragraph::new(message)
                .block(block)
                .style(theme.muted_style())
                .alignment(Alignment::Center),
            area,
        );
        return;
    }

    let selected = app
        .selected_story_index()
        .and_then(|index| index.checked_sub(offset))
        .filter(|index| *index < items.len());
    let mut state = ListState::default().with_selected(selected);
    let highlight_style = if app.focus() == FocusPane::Stories {
        theme.selected_style()
    } else {
        theme.muted_style().add_modifier(Modifier::BOLD)
    };
    let list = List::new(items)
        .block(block)
        .highlight_symbol("▸ ")
        .highlight_style(highlight_style);
    frame.render_stateful_widget(list, area, &mut state);
}

fn story_row(item: &Item, bookmarked: bool, theme: &Theme) -> ListItem<'static> {
    let marker = if bookmarked { "★ " } else { "  " };
    let title_style = if item.is_unavailable() {
        theme.muted_style().add_modifier(Modifier::CROSSED_OUT)
    } else {
        Style::default().fg(theme.foreground)
    };
    let title = Line::from(vec![
        Span::styled(marker, Style::default().fg(theme.warning)),
        Span::styled(sanitize_single_line(item.display_title()), title_style),
    ]);

    let author = sanitize_single_line(item.by.as_deref().unwrap_or("unknown"));
    let domain = sanitize_single_line(
        item.url
            .as_deref()
            .and_then(hostname)
            .unwrap_or("news.ycombinator.com"),
    );
    let age = age(item.time);
    let metadata = if age.is_empty() {
        format!(
            "  {} pts · {} comments · {author} · {domain}",
            item.score, item.descendants
        )
    } else {
        format!(
            "  {} pts · {} comments · {author} · {age} · {domain}",
            item.score, item.descendants
        )
    };
    ListItem::new(vec![title, Line::styled(metadata, theme.muted_style())])
}

fn render_thread(frame: &mut Frame<'_>, area: Rect, app: &App, capacity: usize, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let visible_count = app.comment_count();
    let total_count = app.flattened_comments().len();
    let title = if visible_count == total_count {
        format!(" Thread ({total_count}) ")
    } else {
        format!(" Thread ({visible_count}/{total_count}) ")
    };
    let block = pane_block(title, app.focus() == FocusPane::Thread, theme);
    if app.thread().is_none() {
        let message = if app.loading() {
            "Loading thread…"
        } else {
            "Press Enter on a story to load its thread."
        };
        frame.render_widget(
            Paragraph::new(message)
                .block(block)
                .style(theme.muted_style())
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let offset = app.comment_offset();
    let comments: Vec<ListItem<'_>> = app
        .visible_comment_window(offset, capacity)
        .map(|comment| comment_row(comment, app.is_comment_collapsed(comment.id), theme))
        .collect();
    if comments.is_empty() {
        frame.render_widget(
            Paragraph::new("No comments yet.")
                .block(block)
                .style(theme.muted_style()),
            area,
        );
        return;
    }

    let selected = app
        .selected_comment_index()
        .and_then(|index| index.checked_sub(offset))
        .filter(|index| *index < comments.len());
    let mut state = ListState::default().with_selected(selected);
    let highlight_style = if app.focus() == FocusPane::Thread {
        theme.selected_style()
    } else {
        theme.muted_style().add_modifier(Modifier::BOLD)
    };
    let list = List::new(comments)
        .block(block)
        .highlight_symbol("▸ ")
        .highlight_style(highlight_style);
    frame.render_stateful_widget(list, area, &mut state);
}

fn comment_row(comment: &Comment, collapsed: bool, theme: &Theme) -> ListItem<'static> {
    let depth = usize::try_from(comment.depth.min(8)).unwrap_or(8);
    let indent = "  ".repeat(depth);
    let fold = if collapsed {
        "▸"
    } else if comment.kids.is_empty() && comment.children.is_empty() {
        "·"
    } else {
        "▾"
    };
    let author = if comment.is_unavailable() {
        "[deleted]".to_owned()
    } else {
        sanitize_single_line(comment.by.as_deref().unwrap_or("unknown"))
    };
    let age = age(comment.time);
    let metadata = if age.is_empty() {
        format!("{indent}{fold} {author}")
    } else {
        format!("{indent}{fold} {author} · {age}")
    };
    let body = comment
        .text
        .as_deref()
        .map(strip_html)
        .map(|body| sanitize_single_line(&body))
        .filter(|body| !body.is_empty())
        .unwrap_or_else(|| "[deleted]".to_owned());
    ListItem::new(vec![
        Line::styled(metadata, theme.accent_style()),
        Line::styled(
            format!("{indent}  {body}"),
            Style::default().fg(theme.foreground),
        ),
    ])
}

fn render_detail(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let block = pane_block(
        " Detail / article ",
        app.focus() == FocusPane::Detail,
        theme,
    );
    let mut lines = Vec::new();

    if let Some(article) = app.article() {
        lines.push(Line::styled(
            sanitize_single_line(&article.title),
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ));
        if let Some(url) = &article.url {
            lines.push(Line::styled(sanitize_single_line(url), theme.link_style()));
        }
        lines.push(Line::raw(""));
        lines.extend(
            sanitize_text(&article.body)
                .lines()
                .map(|line| Line::raw(line.to_owned())),
        );
    } else if let Some(comment) = app.selected_comment() {
        lines.extend(comment_detail(comment, theme));
    } else if let Some(item) = app.selected_item() {
        lines.extend(item_detail(item, theme));
    } else {
        lines.push(Line::styled(
            "Select a story to see its details.",
            theme.muted_style(),
        ));
    }

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll(), 0)),
        area,
    );
}

fn comment_detail(comment: &Comment, theme: &Theme) -> Vec<Line<'static>> {
    let author = if comment.is_unavailable() {
        "[deleted]".to_owned()
    } else {
        sanitize_single_line(comment.by.as_deref().unwrap_or("unknown"))
    };
    let age = age(comment.time);
    let heading = if age.is_empty() {
        format!("{author} · comment {}", comment.id)
    } else {
        format!("{author} · {age} · comment {}", comment.id)
    };
    let mut lines = vec![Line::styled(heading, theme.accent_style()), Line::raw("")];
    let body = comment
        .text
        .as_deref()
        .map(strip_html)
        .map(|body| sanitize_text(&body))
        .filter(|body| !body.is_empty())
        .unwrap_or_else(|| "[deleted]".to_owned());
    lines.extend(body.lines().map(|line| Line::raw(line.to_owned())));
    lines
}

fn item_detail(item: &Item, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(
        sanitize_single_line(item.display_title()),
        Style::default()
            .fg(theme.foreground)
            .add_modifier(Modifier::BOLD),
    )];
    lines.push(Line::styled(
        format!(
            "{} points · {} comments · by {}{}",
            item.score,
            item.descendants,
            sanitize_single_line(item.by.as_deref().unwrap_or("unknown")),
            age_suffix(item.time)
        ),
        theme.muted_style(),
    ));
    if let Some(url) = &item.url {
        lines.push(Line::styled(sanitize_single_line(url), theme.link_style()));
    }
    if let Some(body) = &item.text {
        lines.push(Line::raw(""));
        let body = sanitize_text(&strip_html(body));
        lines.extend(body.lines().map(|line| Line::raw(line.to_owned())));
    } else {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Press a to read here or o to open in your browser.",
            theme.muted_style(),
        ));
    }
    lines
}

fn render_status(frame: &mut Frame<'_>, area: Rect, app: &App, mode: LayoutMode, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let mut state = Vec::new();
    if app.offline() {
        state.push(Span::styled(
            " OFFLINE ",
            Style::default()
                .fg(theme.background)
                .bg(theme.warning)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(source) = app.source() {
        state.push(Span::styled(
            format!(" {} ", source.label()),
            theme.accent_style(),
        ));
    }
    if app.stale() {
        state.push(Span::styled(
            " STALE ",
            Style::default()
                .fg(theme.background)
                .bg(theme.warning)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if app.thread_partial() {
        state.push(Span::styled(
            " PARTIAL ",
            Style::default()
                .fg(theme.background)
                .bg(theme.warning)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if app.loading() {
        state.push(Span::styled(" loading… ", theme.accent_style()));
    }
    if let Some(error) = app.error() {
        state.push(Span::styled(
            format!(" {} ", sanitize_single_line(error)),
            theme.error_style(),
        ));
    } else if let Some(message) = app.status() {
        state.push(Span::styled(
            format!(" {} ", sanitize_single_line(message)),
            theme.muted_style(),
        ));
    }

    let focus = format!("{} · {}", mode_label(mode), app.focus().label());
    state.push(Span::styled(format!(" {focus} "), theme.muted_style()));

    let mut lines = vec![Line::from(state)];
    if area.height > 1 {
        let keys = if area.width >= WIDE_MIN_WIDTH {
            " ? help  q quit  j/k move  Ctrl+U/D half-page  PgUp/PgDn page  Tab pane  Enter select  a read  o open "
        } else if area.width >= MEDIUM_MIN_WIDTH {
            " ? help  q quit  Ctrl+U/D half-page  PgUp/PgDn page  Tab pane "
        } else {
            " ? help  q quit  Ctrl+U/D half-page  PgUp/PgDn page "
        };
        lines.push(Line::styled(keys, theme.muted_style()));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_help(frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
    let popup = centered_rect(area, 74, 20);
    if popup.is_empty() {
        return;
    }
    frame.render_widget(Clear, popup);
    let help = vec![
        Line::styled("Navigation", theme.accent_style()),
        Line::raw("j/k or ↑/↓   move selection / scroll article"),
        Line::raw("h/l or ←/→   move between panes"),
        Line::raw("Tab           next pane"),
        Line::raw("Ctrl+U/D      half-page up/down"),
        Line::raw("PgUp/PgDn     full page up/down"),
        Line::raw("Enter         load story thread / fold comment"),
        Line::raw("[/] or 1–6    switch feed"),
        Line::raw(""),
        Line::styled("Find and save", theme.accent_style()),
        Line::raw("/             search Hacker News"),
        Line::raw("f             case-insensitive regex filter"),
        Line::raw("b / Space     toggle bookmark"),
        Line::raw("B             show only bookmarks"),
        Line::raw(""),
        Line::raw("a article · o browser · O offline · r refresh · ? close help · q quit"),
    ];
    frame.render_widget(
        Paragraph::new(help)
            .block(
                Block::default()
                    .title(" Keyboard help ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.accent)),
            )
            .style(Style::default().fg(theme.foreground).bg(theme.background))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn render_prompt(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    let popup = centered_rect(area, 72, 3);
    let Some(prompt) = app.prompt() else {
        return;
    };
    if popup.is_empty() {
        return;
    }
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(format!("{}█", sanitize_single_line(&prompt.value)))
            .block(
                Block::default()
                    .title(format!(
                        " {} · Enter apply · Esc cancel ",
                        prompt.kind.label()
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.accent)),
            )
            .style(Style::default().fg(theme.foreground).bg(theme.background)),
        popup,
    );
}

fn pane_block(title: impl Into<String>, focused: bool, theme: &Theme) -> Block<'static> {
    let border = if focused {
        theme.highlight
    } else {
        theme.border
    };
    let mut style = Style::default().fg(border);
    if focused {
        style = style.add_modifier(Modifier::BOLD);
    }
    Block::default()
        .title(title.into())
        .borders(Borders::ALL)
        .border_style(style)
}

fn centered_rect(area: Rect, percent_width: u16, desired_height: u16) -> Rect {
    let horizontal_margin = area.width.saturating_mul(100 - percent_width.min(100)) / 200;
    let width = area
        .width
        .saturating_sub(horizontal_margin.saturating_mul(2));
    let height = desired_height.min(area.height);
    Rect::new(
        area.x.saturating_add(horizontal_margin),
        area.y
            .saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    )
}

fn mode_label(mode: LayoutMode) -> &'static str {
    match mode {
        LayoutMode::Wide | LayoutMode::Medium => "2-pane",
        LayoutMode::Narrow => "1-pane",
    }
}

fn hostname(url: &str) -> Option<&str> {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = without_scheme.split(['/', '?', '#']).next()?;
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    Some(host.strip_prefix("www.").unwrap_or(host))
}

fn age(timestamp: i64) -> String {
    if timestamp <= 0 {
        return String::new();
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(timestamp);
    let seconds = now.saturating_sub(timestamp).max(0);
    if seconds < 60 {
        "now".to_owned()
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else if seconds < 2_592_000 {
        format!("{}d", seconds / 86_400)
    } else {
        format!("{}mo", seconds / 2_592_000)
    }
}

fn age_suffix(timestamp: i64) -> String {
    let value = age(timestamp);
    if value.is_empty() {
        String::new()
    } else {
        format!(" · {value}")
    }
}

fn strip_html(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut in_tag = false;
    let mut pending_space = false;
    for character in input.chars() {
        match character {
            '<' => {
                in_tag = true;
                pending_space = true;
            }
            '>' if in_tag => in_tag = false,
            _ if in_tag => {}
            character if character.is_whitespace() => pending_space = true,
            character => {
                if pending_space && !output.is_empty() {
                    output.push(' ');
                }
                output.push(character);
                pending_space = false;
            }
        }
    }
    output
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    use super::{LayoutMode, layout_for, render};
    use crate::{
        app::{App, FocusPane, SecondaryPane},
        model::{Comment, Feed, Item, Source, StoryPage, Thread},
        theme::Theme,
    };

    fn page() -> StoryPage {
        StoryPage {
            feed: Feed::Top,
            query: None,
            items: vec![Item {
                id: 1,
                by: Some("alice".to_owned()),
                title: Some("A carefully rendered story".to_owned()),
                url: Some("https://example.com/article".to_owned()),
                score: 123,
                descendants: 45,
                ..Item::default()
            }],
            source: Source::Cache,
            stale: true,
            fetched_at: 1,
        }
    }

    #[test]
    fn exact_width_breakpoints_choose_expected_layouts() {
        assert_eq!(LayoutMode::for_width(120), LayoutMode::Wide);
        assert_eq!(LayoutMode::for_width(119), LayoutMode::Medium);
        assert_eq!(LayoutMode::for_width(80), LayoutMode::Medium);
        assert_eq!(LayoutMode::for_width(79), LayoutMode::Narrow);
    }

    #[test]
    fn wide_and_medium_render_stories_with_the_active_secondary_pane() {
        for width in [120, 119, 80] {
            let thread = layout_for(
                Rect::new(0, 0, width, 30),
                FocusPane::Stories,
                SecondaryPane::Thread,
            );
            let stories = thread.stories.expect("stories pane");
            let right = thread.thread.expect("thread pane");
            assert!(thread.detail.is_none());
            assert_eq!(stories.width, width * 44 / 100);
            assert_eq!(right.x, stories.width);
            assert_eq!(right.width, width - stories.width);

            let detail = layout_for(
                Rect::new(0, 0, width, 30),
                FocusPane::Detail,
                SecondaryPane::Detail,
            );
            assert!(detail.stories.is_some() && detail.thread.is_none() && detail.detail.is_some());
        }
    }

    #[test]
    fn narrow_layout_renders_only_the_focused_pane() {
        let narrow = layout_for(
            Rect::new(0, 0, 79, 30),
            FocusPane::Thread,
            SecondaryPane::Detail,
        );
        assert!(narrow.stories.is_none() && narrow.thread.is_some() && narrow.detail.is_none());
    }

    #[test]
    fn rendering_populates_tabs_story_and_source_status() {
        let backend = TestBackend::new(120, 28);
        let mut terminal = Terminal::new(backend).expect("test terminal");
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
            source: Source::Cache,
            stale: true,
            fetched_at: 1,
        });
        terminal
            .draw(|frame| render(frame, &mut app, &Theme::classic()))
            .expect("render succeeds");

        let buffer = terminal.backend().buffer();
        let mut rendered = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                if let Some(cell) = buffer.cell((x, y)) {
                    rendered.push_str(cell.symbol());
                }
            }
            rendered.push('\n');
        }
        assert!(rendered.contains("Top"));
        assert!(rendered.contains("A carefully rendered story"));
        assert!(rendered.contains("Cache"));
        assert!(rendered.contains("STALE"));
        assert!(rendered.contains("PARTIAL"));
    }

    #[test]
    fn every_responsive_boundary_renders_the_expected_mode() {
        for (width, expected_mode) in [
            (120, "2-pane"),
            (119, "2-pane"),
            (80, "2-pane"),
            (79, "1-pane"),
        ] {
            let backend = TestBackend::new(width, 24);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            let mut app = App::new(page());
            terminal
                .draw(|frame| render(frame, &mut app, &Theme::classic()))
                .expect("boundary frame renders");

            let rendered = terminal.backend().buffer().content().iter().fold(
                String::new(),
                |mut output, cell| {
                    output.push_str(cell.symbol());
                    output
                },
            );
            assert!(
                rendered.contains(expected_mode),
                "width {width} should render {expected_mode}"
            );
            assert!(rendered.contains("? help"));
            assert!(rendered.contains("q quit"));
        }
    }
}
