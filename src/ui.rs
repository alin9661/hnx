//! Responsive Ratatui rendering.

use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    app::{App, FocusPane, SecondaryPane},
    layout::{LayoutPreferences, PaneSet, ResolvedMode, resolve_panes},
    model::{Comment, Feed, Item},
    sanitize::{sanitize_single_line, sanitize_text},
    theme::Theme,
};

/// Rectangles used by the renderer. `None` means that pane is hidden at this width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiLayout {
    pub mode: ResolvedMode,
    pub panes: PaneSet,
    pub masthead: Rect,
    pub stories: Option<Rect>,
    pub thread: Option<Rect>,
    pub detail: Option<Rect>,
    pub status: Rect,
}

/// Computes all UI regions without rendering or mutating application state.
#[must_use]
pub fn layout_for(
    area: Rect,
    preferences: &LayoutPreferences,
    focus: FocusPane,
    secondary: SecondaryPane,
) -> UiLayout {
    let header_height = area.height.min(1);
    let remaining = area.height.saturating_sub(header_height);
    let status_height = remaining.min(1);
    let content_height = remaining.saturating_sub(status_height);

    let masthead = Rect::new(area.x, area.y, area.width, header_height);
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

    let resolved = resolve_panes(content, preferences, focus, secondary);

    UiLayout {
        mode: resolved.mode,
        panes: resolved.panes,
        masthead,
        stories: resolved.stories,
        thread: resolved.thread,
        detail: resolved.detail,
        status,
    }
}

/// Draws the complete interface. Rendering has no clock/tick dependency; it records the current
/// pane visibility, viewport capacities, and Detail extent so subsequent navigation remains
/// aligned with what is on screen.
pub fn render(frame: &mut Frame<'_>, app: &mut App, theme: &Theme) {
    let area = frame.area();
    if area.is_empty() {
        return;
    }

    frame.render_widget(
        Block::default().style(Style::default().bg(theme.background).fg(theme.foreground)),
        area,
    );

    let layout = layout_for(
        area,
        app.layout_preferences(),
        app.focus(),
        app.secondary_pane(),
    );
    let story_rows = layout.stories.map_or(1, |rect| {
        usize::from(rect.height.saturating_sub(1) / 2).max(1)
    });
    let comment_rows = layout.thread.map_or(1, |rect| {
        usize::from(rect.height.saturating_sub(1) / 2).max(1)
    });
    app.set_viewports(story_rows, comment_rows);
    app.set_rendered_panes(layout.panes, layout.mode);

    render_tabs(frame, layout.masthead, app, theme);
    if let Some(rect) = layout.stories {
        let separator = layout.thread.is_some() || layout.detail.is_some();
        render_stories(frame, rect, app, story_rows, separator, theme);
    }
    if let Some(rect) = layout.thread {
        render_thread(
            frame,
            rect,
            app,
            comment_rows,
            layout.detail.is_some(),
            theme,
        );
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

fn render_tabs(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let selected = Feed::ALL
        .iter()
        .position(|feed| *feed == app.feed())
        .unwrap_or_default();
    let masthead_style = Style::default()
        .fg(theme.accent_fg)
        .bg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let brand = " hnx ";
    let labels: Vec<_> = Feed::ALL
        .iter()
        .enumerate()
        .map(|(index, feed)| {
            if index == selected {
                format!(" ◆ {} ", feed.label())
            } else {
                format!("   {} ", feed.label())
            }
        })
        .collect();
    let query_label = app
        .search_query()
        .map(|query| format!(" · “{}” ", sanitize_single_line(query)));
    let full_width = brand.chars().count()
        + labels
            .iter()
            .map(|label| label.chars().count())
            .sum::<usize>()
        + query_label
            .as_ref()
            .map_or(0, |query| Span::raw(query.as_str()).width());
    let visible_labels: Vec<_> = if full_width <= usize::from(area.width) {
        labels.iter().enumerate().collect()
    } else {
        vec![(selected, &labels[selected])]
    };
    let mut spans = vec![Span::styled(brand, masthead_style)];
    for (index, label) in visible_labels {
        let style = if index == selected {
            masthead_style.add_modifier(Modifier::UNDERLINED)
        } else {
            masthead_style
        };
        spans.push(Span::styled(label.clone(), style));
    }
    if let Some(query) = query_label {
        spans.push(Span::styled(query, masthead_style));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(masthead_style),
        area,
    );
}

fn render_stories(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    capacity: usize,
    separator: bool,
    theme: &Theme,
) {
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
    let block = pane_block(title, app.focus() == FocusPane::Stories, separator, theme);

    let offset = app.story_offset();
    let selected = app.selected_story_index();
    let items: Vec<ListItem<'_>> = app
        .visible_item_window(offset, capacity)
        .enumerate()
        .map(|(relative, item)| {
            story_row(
                item,
                app.is_bookmarked(item.id),
                selected == Some(offset.saturating_add(relative)),
                theme,
            )
        })
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

    let selected = selected
        .and_then(|index| index.checked_sub(offset))
        .filter(|index| *index < items.len());
    let mut state = ListState::default().with_selected(selected);
    let highlight_style = Style::default().bg(theme.selected_bg);
    let list = List::new(items)
        .block(block)
        .highlight_symbol("▸ ")
        .highlight_style(highlight_style);
    frame.render_stateful_widget(list, area, &mut state);
}

fn story_row(item: &Item, bookmarked: bool, selected: bool, theme: &Theme) -> ListItem<'static> {
    let marker = if bookmarked { "★ " } else { "  " };
    let title_style = if item.is_unavailable() {
        theme.muted_style().add_modifier(Modifier::CROSSED_OUT)
    } else {
        primary_style(theme, selected)
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
    let mut metadata = vec![Span::styled(
        format!("  {} pts · {} comments · ", item.score, item.descendants),
        theme.muted_style(),
    )];
    metadata.push(Span::styled(author, primary_style(theme, selected)));
    let suffix = if age.is_empty() {
        format!(" · {domain}")
    } else {
        format!(" · {age} · {domain}")
    };
    metadata.push(Span::styled(suffix, theme.muted_style()));
    ListItem::new(vec![title, Line::from(metadata)])
}

fn render_thread(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    capacity: usize,
    separator: bool,
    theme: &Theme,
) {
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
    let block = pane_block(title, app.focus() == FocusPane::Thread, separator, theme);
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
    let selected = app.selected_comment_index();
    let comments: Vec<ListItem<'_>> = app
        .visible_comment_window(offset, capacity)
        .enumerate()
        .map(|(relative, comment)| {
            comment_row(
                comment,
                app.is_comment_collapsed(comment.id),
                selected == Some(offset.saturating_add(relative)),
                theme,
            )
        })
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

    let selected = selected
        .and_then(|index| index.checked_sub(offset))
        .filter(|index| *index < comments.len());
    let mut state = ListState::default().with_selected(selected);
    let highlight_style = Style::default().bg(theme.selected_bg);
    let list = List::new(comments)
        .block(block)
        .highlight_symbol("▸ ")
        .highlight_style(highlight_style);
    frame.render_stateful_widget(list, area, &mut state);
}

fn comment_row(
    comment: &Comment,
    collapsed: bool,
    selected: bool,
    theme: &Theme,
) -> ListItem<'static> {
    let depth = usize::try_from(comment.depth.min(8)).unwrap_or(8);
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
    let mut metadata = depth_rails(depth, theme);
    metadata.push(Span::styled(format!("{fold} "), theme.muted_style()));
    metadata.push(Span::styled(author, primary_style(theme, selected)));
    if !age.is_empty() {
        metadata.push(Span::styled(format!(" · {age}"), theme.muted_style()));
    }
    let body = comment
        .text
        .as_deref()
        .map(strip_html)
        .map(|body| sanitize_single_line(&body))
        .filter(|body| !body.is_empty())
        .unwrap_or_else(|| "[deleted]".to_owned());
    ListItem::new(vec![
        Line::from(metadata),
        Line::from(
            [
                depth_rails(depth, theme),
                vec![
                    Span::styled("  ", theme.muted_style()),
                    Span::styled(body, primary_style(theme, selected)),
                ],
            ]
            .concat(),
        ),
    ])
}

fn primary_style(theme: &Theme, selected: bool) -> Style {
    if selected {
        theme.primary_style().fg(theme.selected_fg)
    } else {
        theme.primary_style()
    }
}

fn depth_rails(depth: usize, theme: &Theme) -> Vec<Span<'static>> {
    (0..depth)
        .map(|level| Span::styled("│ ", theme.depth_style(level)))
        .collect()
}

fn render_detail(frame: &mut Frame<'_>, area: Rect, app: &mut App, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let block = pane_block(
        " Detail / article ",
        app.focus() == FocusPane::Detail,
        false,
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
                .map(|line| Line::styled(line.to_owned(), theme.primary_style())),
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

    let text = Text::from(lines);
    let inner = block.inner(area);
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
    let content_rows = paragraph.line_count(inner.width);
    app.set_detail_metrics(usize::from(inner.height), content_rows);
    let paragraph = paragraph.block(block).scroll((app.detail_scroll(), 0));
    frame.render_widget(paragraph, area);
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
    lines.extend(
        body.lines()
            .map(|line| Line::styled(line.to_owned(), theme.primary_style())),
    );
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
        lines.extend(
            body.lines()
                .map(|line| Line::styled(line.to_owned(), theme.primary_style())),
        );
    } else {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Press a to read here or o to open in your browser.",
            theme.muted_style(),
        ));
    }
    lines
}

fn render_status(frame: &mut Frame<'_>, area: Rect, app: &App, mode: ResolvedMode, theme: &Theme) {
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

    let hints = match (mode, area.width) {
        (ResolvedMode::One, _) => "? help · q quit",
        (_, 100..) => "? help · q quit · Tab pane · L layout · Alt+h/l resize · Alt+0 reset",
        (_, 64..) => "? help · q quit · Tab pane · L layout",
        _ => "? help · q quit",
    };
    let suffix = status_suffix(mode, app.focus(), hints, area.width);
    let suffix_width = u16::try_from(suffix.chars().count())
        .unwrap_or(u16::MAX)
        .min(area.width);
    let message_width = area.width.saturating_sub(suffix_width);
    let message_area = Rect::new(area.x, area.y, message_width, area.height);
    let suffix_area = Rect::new(
        area.x.saturating_add(message_width),
        area.y,
        suffix_width,
        area.height,
    );

    frame.render_widget(Paragraph::new(Line::from(state)), message_area);
    frame.render_widget(
        Paragraph::new(suffix)
            .style(theme.muted_style())
            .alignment(Alignment::Right),
        suffix_area,
    );
}

fn render_help(frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
    let popup = centered_rect(area, 74, 24);
    if popup.is_empty() {
        return;
    }
    frame.render_widget(Clear, popup);
    let full_help = vec![
        Line::styled("Navigation", theme.accent_style()),
        Line::raw("j/k or ↑/↓   move selection / scroll article"),
        Line::raw("h/l or ←/→   move between panes"),
        Line::raw("Tab           next pane"),
        Line::raw("BackTab       previous pane"),
        Line::raw("Ctrl+U/D      half-page up/down"),
        Line::raw("PgUp/PgDn     full page up/down"),
        Line::raw("Enter         load story thread / fold comment"),
        Line::raw("[/] or 1–6    switch feed"),
        Line::raw(""),
        Line::styled("Layout", theme.accent_style()),
        Line::raw("L             toggle two / three panes"),
        Line::raw("Alt+h / Alt+l shrink / grow focused pane"),
        Line::raw("Alt+0         restore config / built-in splits"),
        Line::raw(""),
        Line::styled("Find and save", theme.accent_style()),
        Line::raw("/             search Hacker News"),
        Line::raw("f             case-insensitive regex filter"),
        Line::raw("b / Space     toggle bookmark"),
        Line::raw("B             show only bookmarks"),
        Line::raw(""),
        Line::raw("a article · o browser · O offline · r refresh · ? close help · q quit"),
    ];
    let compact_help = vec![
        Line::styled("Navigation", theme.accent_style()),
        Line::raw("j/k move · h/l pane · Tab cycle · Enter open/fold"),
        Line::styled("Layout", theme.accent_style()),
        Line::raw("L toggle · Alt+h/l resize · Alt+0 reset"),
        Line::styled("Find and save", theme.accent_style()),
        Line::raw("/ search · f filter · b save · B saved only"),
        Line::raw("a article · o browser · O offline · r refresh"),
    ];
    let narrow_help = vec![
        Line::styled("?/Esc close", theme.accent_style()),
        Line::raw("Nav j/k h/l"),
        Line::raw("Tab cycle"),
        Line::raw("Enter open"),
        Line::raw("Layout L"),
        Line::raw("Resize Alt-h/l"),
        Line::raw("Find / f"),
        Line::raw("Save b B"),
        Line::raw("Read a o"),
        Line::raw("More O r"),
    ];
    let help = if area.width < 40 {
        narrow_help
    } else if area.height < 24 {
        compact_help
    } else {
        full_help
    };
    frame.render_widget(
        Paragraph::new(help)
            .block(
                Block::default()
                    .title(" Keyboard help · Esc/? close ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.highlight)),
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
                    .border_style(Style::default().fg(theme.highlight)),
            )
            .style(Style::default().fg(theme.foreground).bg(theme.background)),
        popup,
    );
}

fn pane_block(
    title: impl Into<String>,
    focused: bool,
    separator: bool,
    theme: &Theme,
) -> Block<'static> {
    let border = if focused { theme.accent } else { theme.border };
    let title_style = if focused {
        theme
            .base_style()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        theme.primary_style()
    };
    let title = format!("{}{}", if focused { "◆ " } else { "  " }, title.into());
    let borders = if separator {
        Borders::TOP | Borders::RIGHT
    } else {
        Borders::TOP
    };
    Block::default()
        .title(Line::styled(title, title_style))
        .borders(borders)
        .border_style(Style::default().fg(border))
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

fn mode_label(mode: ResolvedMode) -> &'static str {
    match mode {
        ResolvedMode::Three => "3-pane",
        ResolvedMode::Two => "2-pane",
        ResolvedMode::One => "1-pane",
    }
}

fn status_suffix(mode: ResolvedMode, focus: FocusPane, hints: &str, width: u16) -> String {
    let recovery = " ? help · q quit ";
    let compact_recovery = " ? · q ";
    let focus_and_recovery = format!(" {} ·{recovery}", focus.label());
    let full = format!(" {} · {} · {hints} ", mode_label(mode), focus.label());
    for candidate in [full, focus_and_recovery, recovery.to_owned()] {
        if candidate.chars().count() <= usize::from(width) {
            return candidate;
        }
    }
    if compact_recovery.chars().count() <= usize::from(width) {
        compact_recovery.to_owned()
    } else if width > 0 {
        "q".to_owned()
    } else {
        String::new()
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
    use ratatui::{Terminal, backend::TestBackend, layout::Rect, style::Modifier};

    use super::{centered_rect, layout_for, render, status_suffix};
    use crate::{
        app::{App, ArticleView, FocusPane, SecondaryPane},
        layout::{LayoutPreferences, PaneMode, ResolvedMode},
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

    fn find_run(buffer: &ratatui::buffer::Buffer, needle: &str) -> (u16, u16) {
        let symbols: Vec<_> = needle
            .chars()
            .map(|character| character.to_string())
            .collect();
        for y in buffer.area.y..buffer.area.bottom() {
            for x in buffer.area.x..buffer.area.right() {
                if symbols.iter().enumerate().all(|(offset, symbol)| {
                    u16::try_from(offset)
                        .ok()
                        .and_then(|offset| buffer.cell((x.saturating_add(offset), y)))
                        .is_some_and(|cell| cell.symbol() == symbol)
                }) {
                    return (x, y);
                }
            }
        }
        panic!("rendered text `{needle}` was not found");
    }

    fn cells_for<'a>(
        buffer: &'a ratatui::buffer::Buffer,
        text: &str,
    ) -> impl Iterator<Item = &'a ratatui::buffer::Cell> {
        let (x, y) = find_run(buffer, text);
        (0..text.chars().count()).map(move |offset| {
            let offset = u16::try_from(offset).expect("test text fits terminal");
            buffer.cell((x + offset, y)).expect("rendered cell exists")
        })
    }

    #[test]
    fn exact_width_breakpoints_choose_expected_layouts() {
        let preferences = LayoutPreferences::default().with_mode(PaneMode::Three);
        assert_eq!(
            layout_for(
                Rect::new(0, 0, 120, 20),
                &preferences,
                FocusPane::Stories,
                SecondaryPane::Thread
            )
            .mode,
            ResolvedMode::Three
        );
        assert_eq!(
            layout_for(
                Rect::new(0, 0, 119, 20),
                &preferences,
                FocusPane::Stories,
                SecondaryPane::Thread
            )
            .mode,
            ResolvedMode::Two
        );
    }

    #[test]
    fn wide_and_medium_render_stories_with_the_active_secondary_pane() {
        let preferences = LayoutPreferences::default();
        for width in [120, 119, 80] {
            let thread = layout_for(
                Rect::new(0, 0, width, 30),
                &preferences,
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
                &preferences,
                FocusPane::Detail,
                SecondaryPane::Detail,
            );
            assert!(detail.stories.is_some() && detail.thread.is_none() && detail.detail.is_some());
        }
    }

    #[test]
    fn narrow_layout_renders_only_the_focused_pane() {
        let area = Rect::new(0, 0, 79, 30);
        let preferences = LayoutPreferences::default();
        let stories = layout_for(
            area,
            &preferences,
            FocusPane::Stories,
            SecondaryPane::Thread,
        );
        assert!(stories.stories.is_some() && stories.thread.is_none() && stories.detail.is_none());

        let thread = layout_for(area, &preferences, FocusPane::Thread, SecondaryPane::Detail);
        assert!(thread.stories.is_none() && thread.thread.is_some() && thread.detail.is_none());

        let detail = layout_for(area, &preferences, FocusPane::Detail, SecondaryPane::Thread);
        assert!(detail.stories.is_none() && detail.thread.is_none() && detail.detail.is_some());
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
    fn custom_accent_role_renders_the_brand_chip() {
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut search_page = page();
        search_page.query = Some("rust\nterminal".to_owned());
        let mut app = App::new(search_page);
        let mut theme = Theme::classic();
        theme.accent = ratatui::style::Color::Rgb(12, 34, 56);
        theme.accent_fg = ratatui::style::Color::Rgb(240, 241, 242);
        theme.selected_bg = ratatui::style::Color::Rgb(90, 91, 92);

        terminal
            .draw(|frame| render(frame, &mut app, &theme))
            .expect("custom brand frame renders");

        assert!(
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .any(|cell| { cell.bg == theme.accent && cell.fg == theme.accent_fg })
        );
        let rendered =
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .fold(String::new(), |mut output, cell| {
                    output.push_str(cell.symbol());
                    output
                });
        assert!(rendered.contains("rust terminal"));
    }

    #[test]
    fn masthead_fills_every_cell_and_marks_active_feed_without_color_alone() {
        let backend = TestBackend::new(120, 20);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut app = App::new(page());
        let theme = Theme::classic();
        terminal
            .draw(|frame| render(frame, &mut app, &theme))
            .expect("masthead renders");

        let buffer = terminal.backend().buffer();
        for x in 0..buffer.area.width {
            let cell = buffer.cell((x, 0)).expect("masthead cell");
            assert_eq!(cell.bg, theme.accent, "masthead x={x}");
            assert_eq!(cell.fg, theme.accent_fg, "masthead x={x}");
            assert!(cell.modifier.contains(Modifier::BOLD), "masthead x={x}");
        }
        let active: Vec<_> = cells_for(buffer, "Top").collect();
        assert!(
            active
                .iter()
                .all(|cell| cell.modifier.contains(Modifier::UNDERLINED))
        );
        assert!(buffer.content().iter().any(|cell| cell.symbol() == "◆"));
    }

    #[test]
    fn narrow_masthead_keeps_the_active_feed_and_search_visible() {
        let backend = TestBackend::new(50, 12);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut jobs_page = page();
        jobs_page.feed = Feed::Jobs;
        jobs_page.query = Some("rust".to_owned());
        let mut app = App::new(jobs_page);
        terminal
            .draw(|frame| render(frame, &mut app, &Theme::classic()))
            .expect("narrow frame renders");
        let buffer = terminal.backend().buffer();

        assert!(cells_for(buffer, "Jobs").all(|cell| {
            cell.modifier.contains(Modifier::BOLD) && cell.modifier.contains(Modifier::UNDERLINED)
        }));
        assert!(buffer.content().iter().any(|cell| cell.symbol() == "◆"));
        assert!(find_run(buffer, "hnx").0 < find_run(buffer, "Jobs").0);
        assert!(find_run(buffer, "rust").0 > find_run(buffer, "Jobs").0);
    }

    #[test]
    fn masthead_budgets_wide_search_terms_in_terminal_columns() {
        let backend = TestBackend::new(65, 12);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut search_page = page();
        search_page.query = Some("数据库".to_owned());
        let mut app = App::new(search_page);

        terminal
            .draw(|frame| render(frame, &mut app, &Theme::classic()))
            .expect("wide-character search renders");
        let buffer = terminal.backend().buffer();
        let positions = ["数", "据", "库"].map(|symbol| {
            (0..65)
                .find(|x| {
                    buffer
                        .cell((*x, 0))
                        .is_some_and(|cell| cell.symbol() == symbol)
                })
                .expect("wide query glyph is visible")
        });
        let masthead = buffer.content()[..65]
            .iter()
            .fold(String::new(), |mut output, cell| {
                output.push_str(cell.symbol());
                output
            });

        assert_eq!(positions, [16, 18, 20]);
        assert!(!masthead.contains("New"));
    }

    #[test]
    fn classic_primary_metadata_and_selection_styles_are_semantic() {
        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut app = App::new(page());
        let theme = Theme::classic();
        terminal
            .draw(|frame| render(frame, &mut app, &theme))
            .expect("classic frame renders");
        let buffer = terminal.backend().buffer();

        for text in ["A carefully rendered story", "alice"] {
            assert!(cells_for(buffer, text).all(|cell| {
                cell.fg == theme.foreground && cell.modifier.contains(Modifier::BOLD)
            }));
        }
        assert!(
            cells_for(buffer, "123 pts")
                .all(|cell| { cell.fg == theme.muted && cell.modifier.contains(Modifier::DIM) })
        );
        assert!(buffer.content().iter().any(|cell| {
            cell.bg == ratatui::style::Color::Rgb(229, 228, 222)
                && cell.fg == ratatui::style::Color::Rgb(0, 0, 0)
        }));
    }

    #[test]
    fn custom_and_no_color_themes_preserve_visual_cues() {
        let mut custom = Theme::midnight();
        custom.accent = ratatui::style::Color::Rgb(1, 2, 3);
        custom.accent_fg = ratatui::style::Color::Rgb(250, 249, 248);
        custom.selected_fg = ratatui::style::Color::Rgb(247, 17, 219);
        let backend = TestBackend::new(90, 18);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut app = App::new(page());
        terminal
            .draw(|frame| render(frame, &mut app, &custom))
            .expect("custom frame renders");
        assert!((0..90).all(|x| {
            terminal
                .backend()
                .buffer()
                .cell((x, 0))
                .is_some_and(|cell| cell.bg == custom.accent && cell.fg == custom.accent_fg)
        }));
        let custom_buffer = terminal.backend().buffer();
        assert!(
            cells_for(custom_buffer, "A carefully rendered story")
                .all(|cell| cell.fg == custom.selected_fg)
        );
        assert!(cells_for(custom_buffer, "123 pts").all(|cell| cell.fg == custom.muted));

        let backend = TestBackend::new(90, 18);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut app = App::new(page());
        terminal
            .draw(|frame| render(frame, &mut app, &Theme::no_color()))
            .expect("no-color frame renders");
        let buffer = terminal.backend().buffer();
        assert!(buffer.content().iter().all(|cell| {
            cell.fg == ratatui::style::Color::Reset && cell.bg == ratatui::style::Color::Reset
        }));
        assert!(cells_for(buffer, "Top").all(|cell| {
            cell.modifier.contains(Modifier::BOLD) && cell.modifier.contains(Modifier::UNDERLINED)
        }));
        assert!(buffer.content().iter().any(|cell| cell.symbol() == "◆"));
    }

    #[test]
    fn comment_depth_rails_use_semantic_roles_in_three_pane_mode() {
        let backend = TestBackend::new(120, 20);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut app = App::new(page());
        app.configure_layout(
            LayoutPreferences::default().with_mode(PaneMode::Three),
            LayoutPreferences::default(),
        )
        .expect("layout validates");
        app.load_thread(Thread {
            item: Item {
                id: 1,
                ..Item::default()
            },
            comments: vec![
                Comment {
                    id: 10,
                    by: Some("root".to_owned()),
                    text: Some("root comment body".to_owned()),
                    ..Comment::default()
                },
                Comment {
                    id: 11,
                    by: Some("nested".to_owned()),
                    depth: 2,
                    ..Comment::default()
                },
            ],
            source: Source::Cache,
            stale: false,
            fetched_at: 1,
        });
        let theme = Theme::classic();
        terminal
            .draw(|frame| render(frame, &mut app, &theme))
            .expect("three-pane frame renders");
        let rail_colors: Vec<_> = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .filter(|cell| cell.symbol() == "│")
            .map(|cell| cell.fg)
            .collect();
        assert!(rail_colors.contains(&theme.accent));
        assert!(rail_colors.contains(&theme.link));
        assert!(app.visible_panes().contains(FocusPane::Detail));
    }

    #[test]
    fn all_primary_content_categories_render_bold() {
        let backend = TestBackend::new(120, 22);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut app = App::new(page());
        app.configure_layout(
            LayoutPreferences::default().with_mode(PaneMode::Three),
            LayoutPreferences::default(),
        )
        .expect("layout validates");
        app.load_thread(Thread {
            item: Item {
                id: 1,
                ..Item::default()
            },
            comments: vec![Comment {
                id: 10,
                by: Some("commenter".to_owned()),
                text: Some("comment body text".to_owned()),
                ..Comment::default()
            }],
            source: Source::Cache,
            stale: false,
            fetched_at: 1,
        });
        app.set_article(ArticleView::new(
            "Article heading",
            Some("https://example.com/read".to_owned()),
            "article body text",
        ));
        terminal
            .draw(|frame| render(frame, &mut app, &Theme::classic()))
            .expect("three-pane content renders");
        let buffer = terminal.backend().buffer();

        for text in [
            "Stories",
            "Thread",
            "Detail / article",
            "commenter",
            "comment body text",
            "Article heading",
            "article body text",
        ] {
            assert!(
                cells_for(buffer, text).all(|cell| cell.modifier.contains(Modifier::BOLD)),
                "{text} should be bold"
            );
        }
        assert!(
            cells_for(buffer, "https://example.com/read")
                .all(|cell| cell.modifier.contains(Modifier::UNDERLINED))
        );
    }

    #[test]
    fn every_responsive_boundary_renders_the_expected_mode() {
        for (width, expected_mode, expects_tab_hint) in [
            (120, "2-pane", true),
            (119, "2-pane", true),
            (80, "2-pane", true),
            (79, "1-pane", false),
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
            assert_eq!(rendered.contains("Tab pane"), expects_tab_hint);
        }
    }

    #[test]
    fn long_status_messages_never_hide_focus_or_recovery_hints() {
        for width in [50, 79, 80] {
            let backend = TestBackend::new(width, 12);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            let mut app = App::new(page());
            app.set_error("a very long recoverable error ".repeat(12));
            terminal
                .draw(|frame| render(frame, &mut app, &Theme::classic()))
                .expect("status frame renders");
            let rendered = terminal.backend().buffer().content().iter().fold(
                String::new(),
                |mut output, cell| {
                    output.push_str(cell.symbol());
                    output
                },
            );

            assert!(rendered.contains("stories"), "focus missing at {width}");
            assert!(rendered.contains("? help"), "help missing at {width}");
            assert!(rendered.contains("q quit"), "quit missing at {width}");
        }
    }

    #[test]
    fn ultra_narrow_status_prioritizes_recovery_then_focus_then_mode() {
        let full = status_suffix(
            ResolvedMode::One,
            FocusPane::Stories,
            "? help · q quit",
            u16::MAX,
        );
        let full_width = u16::try_from(full.chars().count()).expect("suffix width fits");
        for (width, expected, omitted) in [
            (20, "? help · q quit", "stories"),
            (30, "stories", "1-pane"),
            (full_width, "1-pane", "never omitted"),
        ] {
            let backend = TestBackend::new(width, 8);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            let mut app = App::new(page());
            terminal
                .draw(|frame| render(frame, &mut app, &Theme::classic()))
                .expect("narrow status renders");
            let rendered = terminal.backend().buffer().content().iter().fold(
                String::new(),
                |mut output, cell| {
                    output.push_str(cell.symbol());
                    output
                },
            );
            assert!(rendered.contains(expected), "missing {expected} at {width}");
            if omitted != "never omitted" {
                assert!(
                    !rendered.contains(omitted),
                    "unexpected {omitted} at {width}"
                );
            }
        }

        for width in [1, 2, 6, 7, 16] {
            let backend = TestBackend::new(width, 3);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            let mut app = App::new(page());
            terminal
                .draw(|frame| render(frame, &mut app, &Theme::classic()))
                .expect("tiny status renders");
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .any(|cell| cell.symbol() == "q"),
                "quit cue missing at width {width}"
            );
        }
    }

    #[test]
    fn selected_comment_uses_custom_primary_foreground_but_muted_fold() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut app = App::new(page());
        app.load_thread(Thread {
            item: Item {
                id: 1,
                ..Item::default()
            },
            comments: vec![Comment {
                id: 10,
                by: Some("selected-commenter".to_owned()),
                text: Some("selected comment body".to_owned()),
                ..Comment::default()
            }],
            source: Source::Cache,
            stale: false,
            fetched_at: 1,
        });
        let mut theme = Theme::midnight();
        theme.selected_fg = ratatui::style::Color::Rgb(247, 17, 219);
        terminal
            .draw(|frame| render(frame, &mut app, &theme))
            .expect("selected comment renders");
        let buffer = terminal.backend().buffer();

        assert!(cells_for(buffer, "selected-commenter").all(|cell| cell.fg == theme.selected_fg));
        assert!(
            cells_for(buffer, "selected comment body").all(|cell| cell.fg == theme.selected_fg)
        );
        let (author_x, author_y) = find_run(buffer, "selected-commenter");
        assert_eq!(
            buffer
                .cell((author_x.saturating_sub(2), author_y))
                .map(|cell| cell.fg),
            Some(theme.muted)
        );
    }

    #[test]
    fn short_help_keeps_dismissal_and_all_categories_visible() {
        for height in [12, 20] {
            let backend = TestBackend::new(100, height);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            let mut app = App::new(page());
            let _ = app.handle_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('?'),
                crossterm::event::KeyModifiers::NONE,
            ));
            terminal
                .draw(|frame| render(frame, &mut app, &Theme::classic()))
                .expect("short help renders");
            let rendered = terminal.backend().buffer().content().iter().fold(
                String::new(),
                |mut output, cell| {
                    output.push_str(cell.symbol());
                    output
                },
            );
            for expected in ["Esc/? close", "Navigation", "Layout", "Find and save"] {
                assert!(
                    rendered.contains(expected),
                    "missing {expected} at height {height}"
                );
            }
        }
    }

    #[test]
    fn narrow_short_help_keeps_its_dismissal_cue_visible() {
        for width in [20, 30] {
            let backend = TestBackend::new(width, 12);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            let mut app = App::new(page());
            let _ = app.handle_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('?'),
                crossterm::event::KeyModifiers::NONE,
            ));
            terminal
                .draw(|frame| render(frame, &mut app, &Theme::classic()))
                .expect("narrow help renders");
            let rendered = terminal.backend().buffer().content().iter().fold(
                String::new(),
                |mut output, cell| {
                    output.push_str(cell.symbol());
                    output
                },
            );
            for expected in ["?/Esc close", "Nav j/k", "Layout L", "Find / f"] {
                assert!(
                    rendered.contains(expected),
                    "missing {expected} at width {width}"
                );
            }
        }
    }

    #[test]
    fn render_applies_brand_styles_and_exposes_shortcuts() {
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut app = App::new(page());
        app.set_focus(FocusPane::Detail);
        let theme = Theme::classic();

        terminal
            .draw(|frame| render(frame, &mut app, &theme))
            .expect("detail frame renders");

        let buffer = terminal.backend().buffer();
        assert!(
            buffer
                .content()
                .iter()
                .any(|cell| { cell.fg == theme.selected_fg && cell.bg == theme.selected_bg })
        );
        let rendered = buffer
            .content()
            .iter()
            .fold(String::new(), |mut output, cell| {
                output.push_str(cell.symbol());
                output
            });
        assert!(rendered.contains("L layout"));
        assert!(rendered.contains("Alt+h/l resize"));
        assert!(rendered.contains("Press a to read here"));

        let detail = layout_for(
            Rect::new(0, 0, 120, 24),
            app.layout_preferences(),
            FocusPane::Detail,
            SecondaryPane::Detail,
        )
        .detail
        .expect("detail pane");
        assert_eq!(
            buffer.cell((detail.x, detail.y)).map(|cell| cell.fg),
            Some(theme.accent)
        );

        let _ = app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::PageDown,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.detail_scroll(), 0);

        let _ = app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('?'),
            crossterm::event::KeyModifiers::NONE,
        ));
        terminal
            .draw(|frame| render(frame, &mut app, &theme))
            .expect("help frame renders");
        let help_popup = centered_rect(Rect::new(0, 0, 120, 24), 74, 20);
        assert_eq!(
            terminal
                .backend()
                .buffer()
                .cell((help_popup.x, help_popup.y))
                .map(|cell| cell.fg),
            Some(theme.highlight)
        );
        let help =
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .fold(String::new(), |mut output, cell| {
                    output.push_str(cell.symbol());
                    output
                });
        assert!(help.contains("Ctrl+U/D"));
        assert!(help.contains("PgUp/PgDn"));

        let _ = app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('?'),
            crossterm::event::KeyModifiers::NONE,
        ));
        let _ = app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('/'),
            crossterm::event::KeyModifiers::NONE,
        ));
        terminal
            .draw(|frame| render(frame, &mut app, &theme))
            .expect("prompt frame renders");
        let prompt_popup = centered_rect(Rect::new(0, 0, 120, 24), 72, 3);
        assert_eq!(
            terminal
                .backend()
                .buffer()
                .cell((prompt_popup.x, prompt_popup.y))
                .map(|cell| cell.fg),
            Some(theme.highlight)
        );
    }

    #[test]
    fn long_detail_pages_to_content_end_and_reclamps_after_resize() {
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut app = App::new(page());
        let body = (0..40)
            .map(|index| format!("body-line-{index:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.set_article(ArticleView::new("Long article", None, body));
        let theme = Theme::classic();

        terminal
            .draw(|frame| render(frame, &mut app, &theme))
            .expect("long detail frame renders");
        let page_down = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::PageDown,
            crossterm::event::KeyModifiers::NONE,
        );
        let _ = app.handle_key(page_down);
        let _ = app.handle_key(page_down);
        assert_eq!(app.detail_scroll(), 21);

        terminal
            .draw(|frame| render(frame, &mut app, &theme))
            .expect("last detail page renders");
        let last_page =
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .fold(String::new(), |mut output, cell| {
                    output.push_str(cell.symbol());
                    output
                });
        assert!(last_page.contains("body-line-39"));

        terminal.backend_mut().resize(120, 40);
        terminal
            .draw(|frame| render(frame, &mut app, &theme))
            .expect("resized detail frame renders");
        assert_eq!(app.detail_scroll(), 5);
        let resized =
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .fold(String::new(), |mut output, cell| {
                    output.push_str(cell.symbol());
                    output
                });
        assert!(resized.contains("body-line-39"));
    }

    #[test]
    fn detail_scroll_reflows_when_width_changes() {
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut app = App::new(page());
        let body = format!("{}body-end", "wrapped ".repeat(800));
        app.set_article(ArticleView::new("Wrapped article", None, body));
        let theme = Theme::classic();

        terminal
            .draw(|frame| render(frame, &mut app, &theme))
            .expect("wrapped detail frame renders");
        let page_down = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::PageDown,
            crossterm::event::KeyModifiers::NONE,
        );
        for _ in 0..20 {
            let _ = app.handle_key(page_down);
        }
        let two_panel_scroll = app.detail_scroll();
        assert!(two_panel_scroll > 0);

        terminal.backend_mut().resize(79, 24);
        terminal
            .draw(|frame| render(frame, &mut app, &theme))
            .expect("reflowed detail frame renders");
        assert!(app.detail_scroll() < two_panel_scroll);
        let reflowed =
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .fold(String::new(), |mut output, cell| {
                    output.push_str(cell.symbol());
                    output
                });
        assert!(reflowed.contains("body-end"));
    }
}
