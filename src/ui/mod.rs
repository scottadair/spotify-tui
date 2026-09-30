mod anim;

pub use anim::{Anim, FRAME};

use crate::api::models::Track;
use crate::app::{
    App, Entry, EntryList, FIXED_SIDEBAR_ITEMS, Focus, Now, Section, SidebarItem, Status, TrackList, View,
};
use crate::settings::{FIELDS, Settings};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Flex, Layout, Margin, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Cell, Clear, HighlightSpacing, List, ListItem, ListState, Padding,
        Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table, Wrap,
    },
};
use ratatui_image::{FilterType, Resize, StatefulImage};
use anim::{Seen, Side};
use std::borrow::Cow;
use std::hash::{Hash, Hasher};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const ACCENT: Color = Color::Rgb(30, 215, 96);
/// Secondary text (artists, times): readable but quieter than primary text.
const MUTED: Color = Color::Gray;
/// Tertiary text and chrome (albums, borders, hints).
const DIM: Color = Color::DarkGray;
const WARN: Color = Color::Yellow;

const SELECT_SYMBOL: &str = "▌";
const COLUMN_SPACING: u16 = 2;

/// Below this width only the focused pane is shown (tab / h / l switch between them).
const SINGLE_PANE_WIDTH: u16 = 80;
/// Below this content width the Album column is dropped.
const ALBUM_MIN_WIDTH: u16 = 70;
/// Below this width the player shows compact shuffle / repeat / volume indicators.
const COMPACT_PLAYER_WIDTH: u16 = 100;
/// Player rows normally, and below `SHORT_HEIGHT` terminal rows (where it drops to two).
const PLAYER_HEIGHT: u16 = 4;
const SHORT_HEIGHT: u16 = 18;

pub fn draw(f: &mut Frame, app: &mut App) {
    let now = Seen {
        view: view_key(app.view.as_ref()),
        depth: app.depth(),
        sidebar_focused: app.focus == Focus::Sidebar,
        fullscreen: app.fullscreen,
        up_next: app.up_next_open,
    };
    let before = app.anim.observe(now);
    let inset = |r: Rect| r.inner(Margin { vertical: 1, horizontal: 1 });

    if app.fullscreen {
        let area = f.area();
        let (player, queue) = draw_fullscreen(f, app, area);
        // Up Next comes in from the right like any forward move; closing it brings the player
        // back from the left.
        let transition = match before {
            Some(b) if !b.fullscreen => Some((area, Side::Bottom)),
            Some(b) if b.up_next != now.up_next => {
                if now.up_next { queue.map(|q| (q, Side::Right)) } else { player.map(|p| (p, Side::Left)) }
            }
            _ => None,
        };
        if let Some((area, side)) = transition {
            app.anim.start(inset(area), side);
        }
    } else {
        let player_h = if f.area().height < SHORT_HEIGHT { 2 } else { PLAYER_HEIGHT };
        let [main, player, footer] = Layout::vertical([
            Constraint::Min(5),
            Constraint::Length(player_h),
            Constraint::Length(1),
        ])
        .areas(f.area());
        let single = main.width < SINGLE_PANE_WIDTH;
        let content = if single {
            match app.focus {
                Focus::Sidebar => {
                    draw_sidebar(f, app, main);
                    None
                }
                Focus::Content => {
                    draw_content(f, app, main);
                    Some(main)
                }
            }
        } else {
            let side_w = (main.width / 4).clamp(20, 32);
            let [side, content] =
                Layout::horizontal([Constraint::Length(side_w), Constraint::Min(20)]).areas(main);
            draw_sidebar(f, app, side);
            draw_content(f, app, content);
            Some(content)
        };
        draw_player(f, app, player);
        draw_footer(f, app, footer);

        // Opening goes forward (in from the right), back comes in from the left; in single-pane
        // mode the library sits to the left of the list.
        let transition = match before {
            Some(b) if b.fullscreen => Some((main, Side::Top)),
            Some(b) if b.view != now.view => {
                content.map(|c| (inset(c), if now.depth < b.depth { Side::Left } else { Side::Right }))
            }
            Some(b) if single && b.sidebar_focused != now.sidebar_focused => {
                Some((inset(main), if now.sidebar_focused { Side::Left } else { Side::Right }))
            }
            _ => None,
        };
        if let Some((area, side)) = transition {
            app.anim.start(area, side);
        }
    }
    app.anim.apply(f.buffer_mut());
    if app.help || app.settings.open {
        dim_behind(f);
    }
    if app.help {
        draw_help(f, f.area(), &mut app.help_scroll);
    }
    if app.settings.open {
        draw_settings(f, f.area(), &app.settings);
    }
}

/// Identity of the content view: changes when a different list is shown, not when it fills in.
fn view_key(view: Option<&View>) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    match view {
        None => 0u8.hash(&mut h),
        Some(View::Tracks(l)) => (1u8, &l.title, l.load).hash(&mut h),
        Some(View::Entries(l)) => (2u8, &l.title, l.load).hash(&mut h),
    }
    h.finish()
}

// ---------------------------------------------------------------- shared pieces

fn pane(title: &str, focused: bool) -> Block<'_> {
    let (border, title_style) = if focused {
        (ACCENT, Style::new().fg(ACCENT).bold())
    } else {
        (DIM, Style::new().fg(MUTED).bold())
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border))
        .title(Span::styled(format!(" {title} "), title_style))
}

/// Adds dim right-aligned `info` (e.g. item counts) to the title bar when it fits beside `title`.
fn with_info<'a>(block: Block<'a>, title: &str, info: String, width: u16) -> Block<'a> {
    // Borders, title padding and a gap.
    if title.width() + info.width() + 8 > usize::from(width) {
        return block;
    }
    block.title(Line::styled(format!(" {info} "), DIM).right_aligned())
}

fn highlight(focused: bool) -> Style {
    if focused {
        Style::new().bg(ACCENT).fg(Color::Black).bold()
    } else {
        Style::new().bg(Color::Rgb(50, 50, 50)).bold()
    }
}

/// Centred single-line message for empty or loading panes.
fn placeholder(f: &mut Frame, area: Rect, text: &str) {
    if area.height == 0 {
        return;
    }
    let row = Rect::new(area.x, area.y + area.height / 3, area.width, 1);
    f.render_widget(Paragraph::new(Line::styled(text, DIM)).alignment(Alignment::Center), row);
}

/// Scroll position drawn over the right border of `area`; hidden when everything fits.
fn scrollbar(f: &mut Frame, area: Rect, len: usize, offset: usize, visible: usize, focused: bool) {
    if len <= visible {
        return;
    }
    let mut state = ScrollbarState::new(len - visible).position(offset).viewport_content_length(visible);
    let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(Some("│"))
        .track_style(Style::new().fg(if focused { ACCENT } else { DIM }))
        .thumb_symbol("┃")
        .thumb_style(Style::new().fg(if focused { Color::White } else { MUTED }));
    f.render_stateful_widget(bar, area.inner(Margin { vertical: 1, horizontal: 0 }), &mut state);
}

/// Column widths exactly as `Table` will lay them out (selection gutter always reserved).
fn column_widths<const N: usize>(width: u16, constraints: [Constraint; N]) -> [usize; N] {
    let [_, cols] = Layout::horizontal([Constraint::Length(SELECT_SYMBOL.width() as u16), Constraint::Fill(0)])
        .areas(Rect::new(0, 0, width, 1));
    let rects = Layout::horizontal(constraints).flex(Flex::Start).spacing(COLUMN_SPACING).split(cols);
    std::array::from_fn(|i| usize::from(rects[i].width))
}

/// Share `avail` columns between text columns. None gets more than its `natural` width until all
/// are satisfied; columns that need more split what remains by `weight`. Surplus goes to the last
/// column, which keeps anything after it pinned to the right edge.
fn fit_columns(avail: usize, natural: &[usize], weight: &[usize]) -> Vec<usize> {
    let mut widths = vec![0; natural.len()];
    let mut open: Vec<usize> = (0..natural.len()).collect();
    let mut left = avail;
    while !open.is_empty() {
        let total: usize = open.iter().map(|&i| weight[i]).sum();
        let settled: Vec<usize> =
            open.iter().copied().filter(|&i| natural[i] * total <= left * weight[i]).collect();
        if settled.is_empty() {
            for &i in &open {
                widths[i] = left * weight[i] / total;
            }
            let used: usize = open.iter().map(|&i| widths[i]).sum();
            widths[open[open.len() - 1]] += left - used;
            return widths;
        }
        for i in settled {
            widths[i] = natural[i];
            left -= natural[i];
            open.retain(|&j| j != i);
        }
    }
    if let Some(last) = widths.last_mut() {
        *last += left;
    }
    widths
}

/// Cut `s` to `max` display columns, marking the cut with an ellipsis.
fn truncate(s: &str, max: usize) -> Cow<'_, str> {
    if s.width() <= max {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(max + 3);
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    if max > 0 {
        out.push('…');
    }
    Cow::Owned(out)
}

fn fmt_ms(ms: u32) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

fn fmt_total(ms: u64) -> String {
    let min = ms / 60_000;
    if min >= 60 { format!("{} hr {} min", min / 60, min % 60) } else { format!("{min} min") }
}

fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

// ---------------------------------------------------------------- sidebar

fn sidebar_icon(item: &SidebarItem) -> &'static str {
    match item {
        SidebarItem::Search => "⌕",
        SidebarItem::Browse => "✦",
        SidebarItem::Liked => "♥",
        SidebarItem::Albums => "◎",
        SidebarItem::RecentPlaylists => "↺",
        SidebarItem::Playlist(_) => " ",
    }
}

fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    let has_playlists = app.sidebar.len() > FIXED_SIDEBAR_ITEMS;
    // Borders and the selection symbol.
    let width = usize::from(area.width.saturating_sub(3));
    let mut items: Vec<ListItem> = Vec::with_capacity(app.sidebar.len() + 1);
    for (i, s) in app.sidebar.iter().enumerate() {
        if i == FIXED_SIDEBAR_ITEMS && has_playlists {
            // Non-selectable divider; selection is shifted past it below.
            let label = " Playlists ";
            let rest = width.saturating_sub(label.width() + 1);
            items.push(ListItem::new(Line::from(vec![
                Span::styled("─", DIM),
                Span::styled(label, Style::new().fg(MUTED).bold()),
                Span::styled("─".repeat(rest), DIM),
            ])));
        }
        let line = if i < FIXED_SIDEBAR_ITEMS {
            Line::from(vec![
                Span::styled(format!("{} ", sidebar_icon(s)), ACCENT),
                Span::styled(truncate(s.label(), width.saturating_sub(2)), Style::new().bold()),
            ])
        } else if s.label().is_empty() {
            Line::styled("Untitled", DIM)
        } else {
            Line::raw(truncate(s.label(), width))
        };
        items.push(ListItem::new(line));
    }
    let shift = |i: usize| if has_playlists && i >= FIXED_SIDEBAR_ITEMS { i + 1 } else { i };
    app.sidebar_view.select(app.sidebar_state.selected().map(shift));
    let list = List::new(items)
        .block(pane("Library", focused))
        .highlight_style(highlight(focused))
        .highlight_symbol(SELECT_SYMBOL)
        .highlight_spacing(HighlightSpacing::Always);
    let len = list.len();
    f.render_stateful_widget(list, area, &mut app.sidebar_view);
    let visible = usize::from(area.height.saturating_sub(2));
    scrollbar(f, area, len, app.sidebar_view.offset(), visible, focused);
}

// ---------------------------------------------------------------- content

fn draw_content(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Content;
    // Take the view out so `app` isn't borrowed twice while rendering.
    let Some(mut view) = app.view.take() else {
        let block = pane("", focused);
        let inner = block.inner(area);
        f.render_widget(block, area);
        placeholder(f, inner, "Pick something from your library, or press / to search");
        return;
    };
    match &mut view {
        View::Tracks(l) => {
            app.page = usize::from(area.height.saturating_sub(3));
            draw_tracks(f, l, area, focused, app.now.track.as_ref().map(|t| t.uri.as_str()), "No playable tracks");
        }
        View::Entries(l) => draw_entries(f, app, l, area, focused),
    }
    app.view = Some(view);
}

fn draw_tracks(
    f: &mut Frame,
    l: &mut TrackList,
    area: Rect,
    focused: bool,
    playing_uri: Option<&str>,
    empty: &str,
) {
    let mut block = pane(&l.title, focused);
    if l.loading {
        block = with_info(block, &l.title, format!("loading {}/{}", l.tracks.len(), l.total), area.width);
    } else if !l.tracks.is_empty() {
        let total: u64 = l.tracks.iter().map(|t| u64::from(t.duration_ms)).sum();
        let info = format!("{} · {}", count(l.tracks.len(), "song"), fmt_total(total));
        block = with_info(block, &l.title, info, area.width);
    }
    let inner = block.inner(area);
    if l.tracks.is_empty() {
        f.render_widget(block, area);
        placeholder(f, inner, if l.loading { "Loading…" } else { empty });
        return;
    }

    const TIME_W: u16 = 5;
    let num_w = l.tracks.len().to_string().len() as u16;
    let show_album = inner.width >= ALBUM_MIN_WIDTH;
    let artist_width = |t: &Track| t.artists.iter().map(|a| a.name.width() + 2).sum::<usize>().saturating_sub(2);
    // Widest cell per text column, capped so one long outlier doesn't claim the row.
    let widest = |cell: &dyn Fn(&Track) -> usize, header: &str, cap: usize| {
        l.tracks.iter().map(cell).max().unwrap_or(0).max(header.width()).min(cap)
    };
    let mut natural = vec![widest(&|t| t.name.width(), "Title", 60), widest(&artist_width, "Artist", 40)];
    let mut weight = vec![4, 3];
    if show_album {
        natural.push(widest(&|t| t.album_name().width(), "Album", 50));
        weight.push(3);
    }
    let fixed_cols = 2 + natural.len() as u16;
    let avail = inner
        .width
        .saturating_sub(SELECT_SYMBOL.width() as u16 + num_w + TIME_W + COLUMN_SPACING * (fixed_cols - 1));
    let text_w = fit_columns(usize::from(avail), &natural, &weight);
    let constraints: Vec<Constraint> = std::iter::once(num_w)
        .chain(text_w.iter().map(|&w| w as u16))
        .chain(std::iter::once(TIME_W))
        .map(Constraint::Length)
        .collect();

    let mut header = vec![Cell::from(Line::from("#").right_aligned()), Cell::from("Title"), Cell::from("Artist")];
    if show_album {
        header.push(Cell::from("Album"));
    }
    header.push(Cell::from(Line::from("Time").right_aligned()));
    let header = Row::new(header).style(Style::new().fg(DIM).bold());
    let rows = l.tracks.iter().enumerate().map(|(i, t)| {
        let playing = playing_uri == Some(t.uri.as_str());
        let (num, title_style) = if playing {
            (Span::styled("▶", ACCENT), Style::new().fg(ACCENT).bold())
        } else {
            (Span::styled((i + 1).to_string(), DIM), Style::new())
        };
        let mut cells = Vec::with_capacity(5);
        cells.push(Cell::from(Line::from(num).right_aligned()));
        cells.push(Cell::from(Span::styled(truncate(&t.name, text_w[0]), title_style)));
        cells.push(Cell::from(Span::styled(truncate(&t.artist_line(), text_w[1]).into_owned(), MUTED)));
        if show_album {
            cells.push(Cell::from(Span::styled(truncate(t.album_name(), text_w[2]), DIM)));
        }
        cells.push(Cell::from(Line::styled(fmt_ms(t.duration_ms), DIM).right_aligned()));
        Row::new(cells)
    });
    let table = Table::new(rows, constraints)
        .header(header)
        .column_spacing(COLUMN_SPACING)
        .flex(Flex::Start)
        .block(block)
        .row_highlight_style(highlight(focused))
        .highlight_symbol(SELECT_SYMBOL)
        .highlight_spacing(HighlightSpacing::Always);
    f.render_stateful_widget(table, area, &mut l.state);
    let visible = usize::from(area.height.saturating_sub(3));
    scrollbar(f, area, l.tracks.len(), l.state.offset(), visible, focused);
}

/// Glyph and group heading for an entry.
fn kind(e: &Entry) -> (&'static str, &str) {
    match e {
        Entry::Track(_) => ("♪", "Songs"),
        Entry::Album(_) => ("◎", "Albums"),
        Entry::Artist(_) => ("●", "Artists"),
        Entry::Playlist(_) => ("≡", "Playlists"),
        Entry::Section(Section::Page(c)) => ("›", c.hub.as_deref().unwrap_or("Browse")),
        Entry::Section(Section::Stations | Section::Recent) => ("›", "For You"),
        Entry::Section(Section::Group(_)) => ("›", "Browse"),
        Entry::Station { .. } => ("∿", "Radio"),
    }
}

/// Primary name and optional secondary detail shown beside it.
fn describe(e: &Entry) -> (Cow<'_, str>, Option<String>) {
    let join = |v: &[crate::api::models::ArtistRef]| v.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(", ");
    match e {
        Entry::Track(t) => (Cow::Borrowed(t.name.as_str()), Some(t.artist_line())),
        Entry::Album(a) => (Cow::Borrowed(a.name.as_str()), Some(join(&a.artists))),
        Entry::Artist(a) => (Cow::Borrowed(a.name.as_str()), None),
        Entry::Playlist(p) => {
            let d = strip_tags(&p.description);
            (Cow::Borrowed(p.name.as_str()), (!d.trim().is_empty()).then_some(d))
        }
        Entry::Section(s) => (Cow::Borrowed(s.label()), None),
        Entry::Station { name, .. } => (Cow::Owned(format!("{name} Radio")), None),
    }
}

fn draw_entries(f: &mut Frame, app: &mut App, l: &mut EntryList, area: Rect, focused: bool) {
    let mut list_area = area;
    let editing = l.search.as_ref().is_some_and(|s| s.editing);
    if let Some(s) = &l.search {
        let [input, rest] = Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).areas(area);
        list_area = rest;
        let mut spans = vec![Span::styled("⌕ ", ACCENT)];
        spans.push(Span::styled(s.query.as_str(), Style::new().bold()));
        if s.editing {
            spans.push(Span::styled("▏", ACCENT));
        }
        if s.query.is_empty() {
            let prompt = if s.editing { "What do you want to listen to?" } else { "What do you want to listen to?  (press /)" };
            spans.push(Span::styled(prompt, DIM));
        }
        f.render_widget(Paragraph::new(Line::from(spans)).block(pane("Search", s.editing)), input);
    }

    let (title, noun) = if l.search.is_some() { ("Results", "result") } else { (l.title.as_str(), "item") };
    let mut block = pane(title, focused && !editing);
    if l.loading {
        block = with_info(block, title, "loading…".to_string(), list_area.width);
    } else if !l.entries.is_empty() {
        block = with_info(block, title, count(l.entries.len(), noun), list_area.width);
    }
    let inner = block.inner(list_area);
    app.page = usize::from(inner.height);
    if l.entries.is_empty() {
        f.render_widget(block, list_area);
        let msg = match &l.search {
            _ if l.loading => "Loading…",
            Some(s) if s.query.trim().is_empty() => "Search songs, albums, artists and playlists",
            Some(_) => "No results",
            None => "Nothing here yet",
        };
        placeholder(f, inner, msg);
        return;
    }

    let described: Vec<_> = l.entries.iter().map(describe).collect();
    // Mixed lists (search results, Browse hubs) get a heading per group.
    let mixed = l.entries.windows(2).any(|w| kind(&w[0]).1 != kind(&w[1]).1);
    let has_detail = described.iter().any(|(_, d)| d.is_some());
    let [_, cols_w] = column_widths(inner.width, [Constraint::Length(1), Constraint::Fill(1)]);
    let name_cap = described.iter().map(|(n, _)| n.width()).max().unwrap_or(0).min(cols_w / 2);
    let constraints = if has_detail {
        [Constraint::Length(1), Constraint::Length(name_cap as u16), Constraint::Fill(1)]
    } else {
        [Constraint::Length(1), Constraint::Fill(1), Constraint::Length(0)]
    };
    let [_, name_w, detail_w] = column_widths(inner.width, constraints);

    let mut rows: Vec<Row> = Vec::with_capacity(l.entries.len() + 8);
    // Row index of each entry, and of the heading above it when it starts a group.
    let mut row_of = Vec::with_capacity(l.entries.len());
    let mut heading_of = Vec::with_capacity(l.entries.len());
    for (i, (e, (name, detail))) in l.entries.iter().zip(&described).enumerate() {
        let (glyph, group) = kind(e);
        if mixed && (i == 0 || kind(&l.entries[i - 1]).1 != group) {
            if i > 0 {
                rows.push(Row::new([""]));
            }
            let n = l.entries[i..].iter().take_while(|x| kind(x).1 == group).count();
            heading_of.push(Some(rows.len()));
            rows.push(Row::new([
                Cell::from(""),
                Cell::from(Line::from(vec![
                    Span::styled(group, Style::new().fg(Color::White).bold()),
                    Span::styled(format!("  {n}"), DIM),
                ])),
            ]));
        } else {
            heading_of.push(None);
        }
        row_of.push(rows.len());
        let name = if name.is_empty() {
            Span::styled("Untitled", DIM)
        } else {
            Span::raw(truncate(name, name_w).into_owned())
        };
        let detail = detail.as_deref().map_or(Cow::Borrowed(""), |d| Cow::Owned(truncate(d, detail_w).into_owned()));
        rows.push(Row::new([
            Cell::from(Span::styled(glyph, DIM)),
            Cell::from(name),
            Cell::from(Span::styled(detail, MUTED)),
        ]));
    }

    let sel = l.state.selected().filter(|&i| i < row_of.len());
    l.view.select(sel.map(|i| row_of[i]));
    // Keep a group's heading on screen when its first item is selected.
    if let Some(h) = sel.and_then(|i| heading_of[i])
        && l.view.offset() > h
    {
        *l.view.offset_mut() = h;
    }
    let len = rows.len();
    let table = Table::new(rows, constraints)
        .column_spacing(COLUMN_SPACING)
        .flex(Flex::Start)
        .block(block)
        .row_highlight_style(highlight(focused))
        .highlight_symbol(SELECT_SYMBOL)
        .highlight_spacing(HighlightSpacing::Always);
    f.render_stateful_widget(table, list_area, &mut l.view);
    scrollbar(f, list_area, len, l.view.offset(), usize::from(inner.height), focused);
}

// ---------------------------------------------------------------- player

fn status_icon(status: Status) -> Span<'static> {
    match status {
        Status::Playing => Span::styled("▶", ACCENT),
        Status::Paused => Span::styled("⏸", MUTED),
        Status::Loading => Span::styled("…", MUTED),
        Status::Stopped => Span::styled("■", DIM),
    }
}

/// `0:06 ━━━━━━──────────── 5:12`, exactly `width` columns wide.
fn progress(now: &Now, width: u16) -> Line<'static> {
    let (pos, dur) = (now.position_ms(), now.track.as_ref().map_or(0, |t| t.duration_ms));
    let (left, right) = (fmt_ms(pos), fmt_ms(dur));
    let bar = usize::from(width).saturating_sub(left.len() + right.len() + 2);
    let filled = if dur == 0 { 0 } else { (bar as u64 * u64::from(pos) / u64::from(dur)) as usize }.min(bar);
    Line::from(vec![
        Span::styled(left, MUTED),
        Span::raw(" "),
        Span::styled("━".repeat(filled), ACCENT),
        Span::styled("─".repeat(bar - filled), DIM),
        Span::raw(" "),
        Span::styled(right, MUTED),
    ])
}

/// Shuffle, repeat and volume; lit when active. `compact` drops the words and volume bar.
fn modes(now: &Now, compact: bool) -> Line<'static> {
    let on = |b: bool| Style::new().fg(if b { ACCENT } else { DIM });
    let (repeat, short, repeating) = match (now.repeat_ctx, now.repeat_track) {
        (_, true) => ("↻ one", "↻1", true),
        (true, false) => ("↻ all", "↻", true),
        _ => ("↻ off", "↻", false),
    };
    let vol = now.volume_percent();
    if compact {
        return Line::from(vec![
            Span::styled("⇄", on(now.shuffle)),
            Span::raw("  "),
            Span::styled(short, on(repeating)),
            Span::styled(format!("  vol {vol}%"), MUTED),
        ]);
    }
    const VOL_CELLS: usize = 8;
    let lit = (usize::from(vol) * VOL_CELLS).div_ceil(100).min(VOL_CELLS);
    Line::from(vec![
        Span::styled("⇄ shuffle", on(now.shuffle)),
        Span::raw("   "),
        Span::styled(repeat, on(repeating)),
        Span::raw("   "),
        Span::styled("vol ", MUTED),
        Span::styled("━".repeat(lit), MUTED),
        Span::styled("─".repeat(VOL_CELLS - lit), DIM),
        Span::styled(format!(" {vol:>3}%"), MUTED),
    ])
}

/// Full player: a titled rule, then title, artist · album, progress. Short terminals get two
/// borderless lines instead: title · artist, progress.
fn draw_player(f: &mut Frame, app: &App, area: Rect) {
    let now = &app.now;
    let short = area.height < PLAYER_HEIGHT;
    let (top, sub, bar) = if short {
        let [top, bar] = Layout::vertical([Constraint::Length(1); 2]).areas(area.inner(Margin { vertical: 0, horizontal: 1 }));
        (top, None, bar)
    } else {
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::new().fg(DIM))
            .title(Span::styled(" Now Playing ", Style::new().fg(MUTED).bold()));
        let inner = block.inner(area).inner(Margin { vertical: 0, horizontal: 1 });
        f.render_widget(block, area);
        let [top, sub, bar] = Layout::vertical([Constraint::Length(1); 3]).areas(inner);
        (top, Some(sub), bar)
    };

    let modes = modes(now, area.width < COMPACT_PLAYER_WIDTH);
    let [title, mode_area] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(modes.width() as u16)]).spacing(2).areas(top);
    f.render_widget(Paragraph::new(modes).alignment(Alignment::Right), mode_area);

    let icon = status_icon(now.status);
    match &now.track {
        Some(t) => {
            let name_w = usize::from(title.width).saturating_sub(3);
            let name = truncate(&t.name, name_w);
            let mut line = vec![icon, Span::raw("  "), Span::styled(name.to_string(), Style::new().fg(Color::White).bold())];
            if sub.is_none() {
                let left = name_w.saturating_sub(name.width() + 3);
                if left > 1 {
                    line.push(Span::styled(format!(" · {}", truncate(&t.artists, left)), MUTED));
                }
            }
            f.render_widget(Line::from(line), title);
            if let Some(sub) = sub {
                let sub_w = usize::from(sub.width).saturating_sub(3);
                let detail = format!("{} · {}", t.artists, t.album);
                f.render_widget(
                    Line::from(vec![Span::raw("   "), Span::styled(truncate(&detail, sub_w).into_owned(), MUTED)]),
                    sub,
                );
            }
        }
        None => f.render_widget(Line::from(vec![icon, Span::styled("  Nothing playing", DIM)]), title),
    }
    f.render_widget(progress(now, bar.width), bar);
}

/// Maximised player. → adds the Up Next pane on the right (in place of the player when the
/// terminal is too narrow for both). Returns the player and Up Next areas that were drawn.
fn draw_fullscreen(f: &mut Frame, app: &mut App, area: Rect) -> (Option<Rect>, Option<Rect>) {
    if !app.up_next_open {
        draw_now_playing(f, app, area, true);
        return (Some(area), None);
    }
    let (player, queue) = if area.width < SINGLE_PANE_WIDTH {
        (None, area)
    } else {
        let [p, q] = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);
        (Some(p), q)
    };
    if let Some(p) = player {
        draw_now_playing(f, app, p, false);
    }
    app.page = usize::from(queue.height.saturating_sub(3));
    draw_tracks(f, &mut app.up_next, queue, true, None, "Nothing queued");
    // Over the pane's bottom border, where the single player pane keeps its hints.
    let hints = Rect::new(queue.x + 1, queue.bottom().saturating_sub(1), queue.width.saturating_sub(2), 1);
    f.render_widget(
        Line::from(hint_spans(&[("enter", "play"), ("←/esc", "player"), ("f", "exit"), ("?", "help")], usize::from(hints.width)))
            .centered(),
        hints,
    );
    (player, Some(queue))
}

/// Album art and track details centred vertically above the progress bar.
fn draw_now_playing(f: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    let mut block = pane("Now Playing", focused);
    if focused {
        block = block.title_bottom(
            Line::from(hint_spans(&[("→", "up next"), ("f/esc", "back"), ("?", "help")], usize::MAX)).centered(),
        );
    }
    let inner = block.inner(area);
    f.render_widget(block, area);

    let [body, _, bar, mode] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1), Constraint::Length(1), Constraint::Length(1)])
            .areas(inner);

    // Largest square (in pixels) that fits above the three detail lines; terminal cells are not square.
    const DETAILS_H: u16 = 4;
    let font = app.picker.font_size();
    let (fw, fh) = (u32::from(font.width.max(1)), u32::from(font.height.max(1)));
    let mut h = u32::from(body.height.saturating_sub(DETAILS_H));
    let mut w = h * fh / fw;
    if w > u32::from(body.width) {
        w = u32::from(body.width);
        h = w * fw / fh;
    }
    let (w, h) = (w as u16, h as u16);
    // Art and details as one block, centred in the space above the progress bar.
    let [art, _, title, artist, album] = Layout::vertical([
        Constraint::Length(h),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .flex(Flex::Center)
    .areas(body);
    let art_rect = Rect::new(art.x + (art.width - w) / 2, art.y, w, art.height);
    // Graphics-protocol images aren't cells, so they can't be greyed out behind a popup.
    let popup = app.help || app.settings.open;
    match app.cover.as_mut().and_then(|c| c.proto.as_mut()).filter(|_| !popup) {
        Some(proto) => f.render_stateful_widget(
            StatefulImage::default().resize(Resize::Scale(Some(FilterType::Triangle))),
            art_rect,
            proto,
        ),
        None => f.render_widget(
            Paragraph::new("♪").style(Style::new().fg(DIM)).alignment(Alignment::Center),
            Rect::new(art.x, art.y + art.height / 2, art.width, 1.min(art.height)),
        ),
    }

    let now = &app.now;
    let centered = |line: Line<'static>| Paragraph::new(line).alignment(Alignment::Center);
    match &now.track {
        Some(t) => {
            f.render_widget(
                centered(Line::styled(t.name.clone(), Style::new().fg(Color::White).bold())),
                title,
            );
            f.render_widget(centered(Line::styled(t.artists.clone(), MUTED)), artist);
            f.render_widget(centered(Line::styled(t.album.clone(), DIM)), album);
        }
        None => f.render_widget(centered(Line::styled("Nothing playing", DIM)), title),
    }

    let bar_w = bar.width.min(70);
    let bar = Rect::new(bar.x + (bar.width - bar_w) / 2, bar.y, bar_w, bar.height);
    f.render_widget(progress(now, bar_w), bar);
    f.render_widget(centered(modes(now, inner.width < 60)), mode);
}

// ---------------------------------------------------------------- footer & help

/// `key what  ·  key what`, padded by a space each side. Hints are in priority order; when they
/// don't fit in `max_width`, middle ones are dropped so the last (help) always shows.
fn hint_spans(hints: &[(&'static str, &'static str)], max_width: usize) -> Vec<Span<'static>> {
    const SEP: &str = "  ·  ";
    let cost = |(k, w): &(&str, &str)| k.width() + 1 + w.width();
    let Some((last, rest)) = hints.split_last() else { return Vec::new() };
    let mut used = 2 + cost(last);
    let mut shown: Vec<&(&str, &str)> = Vec::with_capacity(hints.len());
    for h in rest {
        used += cost(h) + SEP.width();
        if used > max_width {
            break;
        }
        shown.push(h);
    }
    shown.push(last);

    let mut spans = Vec::with_capacity(shown.len() * 3 + 2);
    spans.push(Span::raw(" "));
    for (i, (key, what)) in shown.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(SEP, DIM));
        }
        spans.push(Span::styled(*key, Style::new().fg(ACCENT).bold()));
        spans.push(Span::styled(format!(" {what}"), MUTED));
    }
    spans.push(Span::raw(" "));
    spans
}

/// The handful of keys that matter where the user currently is.
fn hints(app: &App) -> &'static [(&'static str, &'static str)] {
    let editing = matches!(&app.view, Some(View::Entries(l)) if l.search.as_ref().is_some_and(|s| s.editing));
    match (&app.view, app.focus) {
        _ if editing => &[("enter", "search"), ("esc", "stop typing")],
        (_, Focus::Sidebar) | (None, _) => &[
            ("enter", "open"),
            ("→", "to list"),
            ("/", "search"),
            ("space", "play/pause"),
            ("f", "player"),
            ("q", "quit"),
            (",", "settings"),
            ("?", "all keys"),
        ],
        (Some(View::Tracks(_)), Focus::Content) => &[
            ("enter", "play"),
            ("R", "radio"),
            ("esc", "back"),
            ("←", "to library"),
            ("space", "play/pause"),
            ("f", "player"),
            ("?", "all keys"),
        ],
        (Some(View::Entries(_)), Focus::Content) => &[
            ("enter", "open"),
            ("R", "radio"),
            ("esc", "back"),
            ("/", "search"),
            ("space", "play/pause"),
            ("f", "player"),
            ("?", "all keys"),
        ],
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let line = match &app.status {
        Some((msg, _)) => Line::from(vec![Span::styled(" ● ", WARN), Span::styled(msg.clone(), WARN)]),
        None => Line::from(hint_spans(hints(app), usize::from(area.width))),
    };
    f.render_widget(Paragraph::new(line), area);
}

// ---------------------------------------------------------------- popups

/// Greys out everything drawn so far so a popup stands out: colours, highlights and bold all go.
fn dim_behind(f: &mut Frame) {
    for cell in &mut f.buffer_mut().content {
        cell.set_style(Style::reset().fg(DIM));
    }
}

/// Border + horizontal padding a popup adds around its content.
const POPUP_CHROME_W: u16 = 2 + 4;

/// Popup size for `content` cells: centred, a little off the screen edges, and with a blank row
/// above and below the content when the terminal is tall enough. Returns the rect and that padding.
fn popup_rect(area: Rect, content_w: u16, content_h: u16) -> (Rect, u16) {
    let pad = u16::from(content_h + 2 + 2 + 2 <= area.height);
    let w = (content_w + POPUP_CHROME_W).min(area.width.saturating_sub(4)).max(area.width.min(20));
    let h = (content_h + 2 + 2 * pad).min(area.height.saturating_sub(2)).max(area.height.min(3));
    (Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h), pad)
}

fn popup_block<'a>(title: &'a str, hints: &[(&'static str, &'static str)], width: u16, pad: u16) -> Block<'a> {
    pane(title, true)
        .title_bottom(Line::from(hint_spans(hints, usize::from(width.saturating_sub(4)))).centered())
        .padding(Padding::new(2, 2, pad, pad))
}

const HELP: &[(&str, &[(&str, &str)])] = &[
    ("Navigate", &[
        ("j k  ↓ ↑", "move selection"),
        ("g G", "top / bottom"),
        ("ctrl-d ctrl-u", "half page down / up"),
        ("← →  h l  tab", "switch pane"),
        ("enter", "open / play"),
        ("esc  backspace", "back"),
        ("/", "search"),
    ]),
    ("Playback", &[
        ("space", "play / pause"),
        ("n p", "next / previous"),
        ("< >", "seek −5s / +5s"),
        ("+ -", "volume"),
        ("s", "shuffle"),
        ("r", "cycle repeat"),
        ("R", "start radio from the selection"),
    ]),
    ("App", &[
        ("f", "full-screen player with album art"),
        ("→  l", "up next (in the full-screen player)"),
        (",", "settings"),
        ("?", "this help"),
        ("q  ctrl-c", "quit"),
    ]),
];
const HELP_COLUMN_GAP: u16 = 4;

/// `HELP` split into `n` columns of consecutive sections, balanced by height.
fn help_columns(n: usize) -> Vec<Vec<Line<'static>>> {
    let heights: Vec<usize> = HELP.iter().map(|(_, keys)| keys.len() + 1).collect();
    let target = (heights.iter().sum::<usize>() + heights.len() - 1).div_ceil(n);
    let mut groups: Vec<std::ops::Range<usize>> = Vec::with_capacity(n);
    let (mut start, mut h) = (0, 0);
    for (i, &sh) in heights.iter().enumerate() {
        if i > start && h + 1 + sh > target && groups.len() + 1 < n {
            groups.push(start..i);
            (start, h) = (i, sh);
        } else {
            h += if i > start { 1 + sh } else { sh };
        }
    }
    groups.push(start..HELP.len());

    groups
        .into_iter()
        .map(|g| {
            let sections = &HELP[g];
            // Descriptions line up across every section in the column.
            let key_w = sections.iter().flat_map(|(_, keys)| keys.iter()).map(|(k, _)| k.width()).max().unwrap_or(0) + 3;
            let mut lines = Vec::new();
            for (i, (title, keys)) in sections.iter().enumerate() {
                if i > 0 {
                    lines.push(Line::raw(""));
                }
                lines.push(Line::styled(*title, Style::new().fg(Color::White).bold()));
                for (k, d) in *keys {
                    lines.push(Line::from(vec![
                        Span::styled(format!("{k:<key_w$}"), Style::new().fg(ACCENT).bold()),
                        Span::styled(*d, MUTED),
                    ]));
                }
            }
            lines
        })
        .collect()
}

fn draw_help(f: &mut Frame, area: Rect, scroll: &mut u16) {
    let size = |cols: &[Vec<Line>]| {
        let w: usize = cols.iter().map(|c| c.iter().map(Line::width).max().unwrap_or(0)).sum();
        let w = w as u16 + HELP_COLUMN_GAP * (cols.len() as u16 - 1);
        (w, cols.iter().map(Vec::len).max().unwrap_or(0) as u16)
    };
    // One column when it fits; otherwise spread sideways, as few columns as get it to fit, or as
    // many as the width allows (and scroll).
    let fits_w = |w: u16| w + POPUP_CHROME_W + 4 <= area.width;
    let fits_h = |h: u16| h + 2 + 2 <= area.height;
    let layouts: Vec<_> = (1..=HELP.len()).map(help_columns).collect();
    let cols = layouts
        .iter()
        .find(|c| {
            let (w, h) = size(c);
            fits_w(w) && fits_h(h)
        })
        .or_else(|| layouts.iter().rev().find(|c| fits_w(size(c).0)))
        .unwrap_or(&layouts[0]);
    let (content_w, content_h) = size(cols);

    let (rect, pad) = popup_rect(area, content_w, content_h);
    // Short terminals: scroll with j/k rather than silently cutting the list off.
    let visible = rect.height.saturating_sub(2 + 2 * pad);
    let hidden = content_h.saturating_sub(visible);
    *scroll = (*scroll).min(hidden);
    let hints: &[_] = if hidden > 0 { &[("j k", "scroll"), ("esc", "close")] } else { &[("esc", "close")] };
    let block = popup_block("Keys", hints, rect.width, pad);
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block, rect);

    let widths = cols.iter().map(|c| Constraint::Length(c.iter().map(Line::width).max().unwrap_or(0) as u16));
    let areas = Layout::horizontal(widths).spacing(HELP_COLUMN_GAP).split(inner);
    for (lines, &col) in cols.iter().zip(areas.iter()) {
        f.render_widget(Paragraph::new(lines.clone()).scroll((*scroll, 0)), col);
    }
}

fn draw_settings(f: &mut Frame, area: Rect, s: &Settings) {
    const PENDING: &str = " ●";
    // Widest value: a client id, or a choice between ‹ › arrows.
    const VALUE_W: usize = 32;
    let label_w = FIELDS.iter().map(|f| f.label().width()).max().unwrap_or(0) + 3;

    let items: Vec<ListItem> = FIELDS
        .iter()
        .enumerate()
        .map(|(i, &field)| {
            let selected = i == s.selected;
            let value = field.value(&s.config);
            let mut spans = vec![Span::styled(format!(" {:<label_w$}", field.label()), MUTED)];
            match &s.input {
                Some(input) if selected => {
                    spans.push(Span::raw(input.clone()));
                    spans.push(Span::styled("▏", ACCENT));
                }
                _ if value.is_empty() => spans.push(Span::styled("not set", DIM)),
                _ if selected && !field.is_text() => spans.push(Span::raw(format!("‹ {value} ›"))),
                _ => spans.push(Span::styled(value, Color::White)),
            }
            if s.pending(field) {
                spans.push(Span::styled(PENDING, WARN));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let (note, note_style) = match &s.error {
        Some(e) => (e.as_str(), Style::new().fg(WARN)),
        None => (s.field().description(), Style::new().fg(MUTED)),
    };
    let list_w = SELECT_SYMBOL.width() + 1 + label_w + VALUE_W + PENDING.width();
    let content_w = FIELDS.iter().map(|f| f.description().width()).max().unwrap_or(0).max(list_w) as u16;
    let (rect, pad) = popup_rect(area, content_w, FIELDS.len() as u16 + 2);

    let hints: &[_] = if s.input.is_some() {
        &[("enter", "save"), ("esc", "cancel")]
    } else if s.field().is_text() {
        &[("↑ ↓", "select"), ("enter", "edit"), ("esc", "close")]
    } else {
        &[("↑ ↓", "select"), ("← →", "change"), ("esc", "close")]
    };
    let mut block = popup_block("Settings", hints, rect.width, pad);
    if FIELDS.iter().any(|&f| s.pending(f)) {
        block = block.title(Line::styled(" ● restart to apply ", WARN).right_aligned());
    }
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block, rect);

    let [list_area, _, note_area] =
        Layout::vertical([Constraint::Length(FIELDS.len() as u16), Constraint::Length(1), Constraint::Min(1)])
            .areas(inner);
    let list = List::new(items)
        .highlight_style(highlight(true))
        .highlight_symbol(SELECT_SYMBOL)
        .highlight_spacing(HighlightSpacing::Always);
    f.render_stateful_widget(list, list_area, &mut ListState::default().with_selected(Some(s.selected)));
    f.render_widget(Paragraph::new(Line::styled(note, note_style)).wrap(Wrap { trim: true }), note_area);
}

/// Editorial descriptions contain markup like `<a href=...>Artist</a>` and HTML entities; keep only the text.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    if out.contains('&') {
        for (entity, c) in [("&amp;", "&"), ("&quot;", "\""), ("&#x27;", "'"), ("&#39;", "'"), ("&lt;", "<"), ("&gt;", ">")] {
            out = out.replace(entity, c);
        }
    }
    out
}
