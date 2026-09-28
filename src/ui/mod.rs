use crate::app::{App, Entry, EntryList, FIXED_SIDEBAR_ITEMS, Focus, Status, TrackList, View};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Cell, Clear, LineGauge, List, ListItem, Paragraph, Row, Table,
    },
};

const ACCENT: Color = Color::Rgb(30, 215, 96);
const DIM: Color = Color::DarkGray;

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main, player, footer] = Layout::vertical([
        Constraint::Min(5),
        Constraint::Length(4),
        Constraint::Length(1),
    ])
    .areas(f.area());
    let [side, content] =
        Layout::horizontal([Constraint::Length(30), Constraint::Min(20)]).areas(main);

    draw_sidebar(f, app, side);
    draw_content(f, app, content);
    draw_player(f, app, player);
    draw_footer(f, app, footer);
    if app.help {
        draw_help(f, f.area());
    }
}

fn pane(title: &str, focused: bool) -> Block<'_> {
    let color = if focused { ACCENT } else { DIM };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color))
        .title(Span::styled(format!(" {title} "), Style::new().fg(color).bold()))
}

fn highlight(focused: bool) -> Style {
    if focused {
        Style::new().bg(ACCENT).fg(Color::Black).bold()
    } else {
        Style::new().bg(Color::Rgb(50, 50, 50)).bold()
    }
}

fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    let items: Vec<ListItem> = app
        .sidebar
        .iter()
        .enumerate()
        .map(|(i, s)| {
            // Playlists (index 3+) are visually separated from fixed entries.
            let style = if i >= FIXED_SIDEBAR_ITEMS { Style::new() } else { Style::new().bold() };
            ListItem::new(s.label().to_string()).style(style)
        })
        .collect();
    let list = List::new(items)
        .block(pane("Library", focused))
        .highlight_style(highlight(focused))
        .highlight_symbol("▌");
    f.render_stateful_widget(list, area, &mut app.sidebar_state);
}

fn draw_content(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Content;
    // Take the view out so `app` isn't borrowed twice while rendering.
    let Some(mut view) = app.view.take() else {
        f.render_widget(pane("", focused), area);
        return;
    };
    match &mut view {
        View::Tracks(l) => {
            app.page = area.height.saturating_sub(4) as usize;
            draw_tracks(f, l, area, focused, app.now.track.as_ref().map(|t| t.uri.as_str()));
        }
        View::Entries(l) => draw_entries(f, app, l, area, focused),
    }
    app.view = Some(view);
}

fn fmt_ms(ms: u32) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

fn draw_tracks(f: &mut Frame, l: &mut TrackList, area: Rect, focused: bool, playing_uri: Option<&str>) {
    let title = if l.loading {
        format!("{} — loading {}/{}", l.title, l.tracks.len(), l.total)
    } else {
        format!("{} — {} tracks", l.title, l.tracks.len())
    };
    let header = Row::new(["#", "Title", "Artist", "Album", "Time"])
        .style(Style::new().fg(DIM).add_modifier(Modifier::BOLD));
    let rows = l.tracks.iter().enumerate().map(|(i, t)| {
        let is_playing = playing_uri == Some(t.uri.as_str());
        let style = if is_playing { Style::new().fg(ACCENT) } else { Style::new() };
        let marker = if is_playing { "▶".to_string() } else { (i + 1).to_string() };
        Row::new([
            Cell::from(marker),
            Cell::from(t.name.as_str()),
            Cell::from(t.artist_line()),
            Cell::from(t.album_name()),
            Cell::from(fmt_ms(t.duration_ms)),
        ])
        .style(style)
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Percentage(38),
            Constraint::Percentage(28),
            Constraint::Percentage(28),
            Constraint::Length(6),
        ],
    )
    .header(header)
    .block(pane(&title, focused))
    .row_highlight_style(highlight(focused));
    f.render_stateful_widget(table, area, &mut l.state);
}

fn draw_entries(f: &mut Frame, app: &mut App, l: &mut EntryList, area: Rect, focused: bool) {
    let mut list_area = area;
    if let Some(s) = &l.search {
        let [input, rest] = Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).areas(area);
        list_area = rest;
        let cursor = if s.editing { "▏" } else { "" };
        let hint = if s.query.is_empty() && !s.editing { "press / to search" } else { "" };
        let text = Line::from(vec![
            Span::raw(s.query.clone()),
            Span::styled(cursor, Style::new().fg(ACCENT)),
            Span::styled(hint, Style::new().fg(DIM)),
        ]);
        f.render_widget(Paragraph::new(text).block(pane("Search", s.editing)), input);
    }
    app.page = list_area.height.saturating_sub(2) as usize;

    let title = if l.loading { format!("{} — loading…", l.title) } else { l.title.clone() };
    let items: Vec<ListItem> = l
        .entries
        .iter()
        .map(|e| {
            let (kind, text) = match e {
                Entry::Track(t) => ("track", format!("{} — {}", t.name, t.artist_line())),
                Entry::Album(a) => (
                    "album",
                    format!(
                        "{} — {}",
                        a.name,
                        a.artists.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(", ")
                    ),
                ),
                Entry::Artist(a) => ("artist", a.name.clone()),
                Entry::Playlist(p) if p.description.is_empty() => ("playlist", p.name.clone()),
                Entry::Playlist(p) => ("playlist", format!("{} — {}", p.name, strip_tags(&p.description))),
                Entry::Section(s) => ("browse", s.label().to_string()),
                Entry::Station { name, .. } => ("radio", format!("{name} Radio")),
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{kind:<9}"), Style::new().fg(DIM)),
                Span::raw(text),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(pane(&title, focused && !l.search.as_ref().is_some_and(|s| s.editing)))
        .highlight_style(highlight(focused))
        .highlight_symbol("▌");
    f.render_stateful_widget(list, list_area, &mut l.state);
}

fn draw_player(f: &mut Frame, app: &App, area: Rect) {
    let now = &app.now;
    let mode = format!(
        " vol {}%  shuffle {}  repeat {} ",
        now.volume_percent(),
        if now.shuffle { "on" } else { "off" },
        match (now.repeat_ctx, now.repeat_track) {
            (_, true) => "track",
            (true, false) => "all",
            _ => "off",
        }
    );
    let block = pane("Now Playing", false).title_bottom(Line::styled(mode, Style::new().fg(DIM)).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [info, bar] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);

    let icon = match now.status {
        Status::Playing => "▶",
        Status::Paused => "⏸",
        Status::Loading => "…",
        Status::Stopped => "■",
    };
    let line = match &now.track {
        Some(t) => Line::from(vec![
            Span::styled(format!("{icon} "), Style::new().fg(ACCENT)),
            Span::styled(t.name.clone(), Style::new().bold()),
            Span::raw(format!("  {}", t.artists)),
            Span::styled(format!("  · {}", t.album), Style::new().fg(DIM)),
        ]),
        None => Line::styled(format!("{icon} Nothing playing"), Style::new().fg(DIM)),
    };
    f.render_widget(Paragraph::new(line), info);

    let (pos, dur) = (now.position_ms(), now.track.as_ref().map_or(0, |t| t.duration_ms));
    let ratio = if dur == 0 { 0.0 } else { (f64::from(pos) / f64::from(dur)).clamp(0.0, 1.0) };
    let gauge = LineGauge::default()
        .filled_style(Style::new().fg(ACCENT))
        .unfilled_style(Style::new().fg(DIM))
        .label(format!("{} / {}", fmt_ms(pos), fmt_ms(dur)))
        .ratio(ratio);
    f.render_widget(gauge, bar);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let line = match &app.status {
        Some((msg, _)) => Line::styled(msg.clone(), Style::new().fg(Color::Yellow)),
        None => Line::styled(
            "space play/pause  n/p next/prev  / search  tab focus  enter play/open  esc back  ? help  q quit",
            Style::new().fg(DIM),
        ),
    };
    f.render_widget(Paragraph::new(line), area);
}

fn draw_help(f: &mut Frame, area: Rect) {
    const HELP: &[(&str, &str)] = &[
        ("j / k, ↓ / ↑", "move selection"),
        ("g / G", "top / bottom"),
        ("ctrl-d / ctrl-u", "half page down / up"),
        ("h / l, tab", "switch pane"),
        ("enter", "open / play"),
        ("esc, backspace", "back"),
        ("/", "search (enter to run, esc to cancel)"),
        ("space", "play / pause"),
        ("n / p", "next / previous"),
        ("< / >", "seek -5s / +5s"),
        ("+ / -", "volume"),
        ("s / r", "shuffle / cycle repeat"),
        ("R", "start radio from selected track/artist/station"),
        ("q, ctrl-c", "quit"),
    ];
    let w = 56.min(area.width);
    let h = (HELP.len() as u16 + 2).min(area.height);
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(k, d)| Line::from(vec![Span::styled(format!("{k:<18}"), Style::new().fg(ACCENT).bold()), Span::raw(*d)]))
        .collect();
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(pane("Help", true)), rect);
}

/// Editorial descriptions contain markup like `<a href=...>Artist</a>`; keep only the text.
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
    out
}
