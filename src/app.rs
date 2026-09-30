use crate::api::{
    Api, HttpError,
    models::{Album, Artist, Playlist, SearchResults, Track},
};
use crate::browse::{self, Category, Group, Item};
use crate::cache::Cache;
use crate::event::{Data, Event};
use crate::player::{PlaybackEvent, PlayerHandle, TrackInfo};
use crate::settings::{Outcome, Settings};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::{ListState, TableState};
use ratatui_image::{picker::Picker, protocol::StatefulProtocol};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

const VOLUME_STEP: u16 = u16::MAX / 20;
/// Wait after a track or shuffle change before asking for the queue, so Spirc's state
/// update (sent ~200 ms later) has reached Spotify.
const QUEUE_SETTLE: Duration = Duration::from_secs(1);
const SEEK_STEP_MS: u32 = 5_000;
const STATUS_TTL: Duration = Duration::from_secs(5);

/// Cached track lists newer than this are shown without touching the network.
const TRACKS_FRESH: Duration = Duration::from_secs(5 * 60);
const SECTION_TTL: Duration = Duration::from_secs(60 * 60);
const CATEGORY_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const TOP_ARTISTS_TTL: Duration = Duration::from_secs(6 * 60 * 60);

// ---------------------------------------------------------------- views

pub struct TrackList {
    pub title: String,
    pub tracks: Vec<Track>,
    /// Item count reported by the API.
    pub total: u32,
    /// Items dropped because they are not playable catalog tracks (episodes, local files).
    pub skipped: u32,
    pub loading: bool,
    pub load: u64,
    /// Context URI (album/playlist) whose item order matches `tracks` exactly, if any.
    pub context: Option<String>,
    pub state: TableState,
    /// Disk cache key; `None` for lists that must always be live.
    cache_key: Option<String>,
    /// `tracks` currently hold cached content that the first live chunk replaces.
    from_cache: bool,
}

impl TrackList {
    fn new(title: String, load: u64, context: Option<String>, cache_key: Option<String>) -> Self {
        Self {
            title,
            tracks: Vec::new(),
            total: 0,
            skipped: 0,
            loading: true,
            load,
            context,
            state: TableState::default(),
            cache_key,
            from_cache: false,
        }
    }
}

/// Persisted form of a fully loaded track list.
#[derive(Serialize, Deserialize)]
pub struct TrackCache {
    tracks: Vec<Track>,
    total: u32,
    skipped: u32,
}

/// A page under Browse.
#[derive(Clone)]
pub enum Section {
    /// A browse page: a top-level category or a sub-page of one.
    Page(Category),
    /// One shelf of a page.
    Group(Group),
    /// Radio stations seeded by your top artists.
    Stations,
    Recent,
}

impl Section {
    pub fn label(&self) -> &str {
        match self {
            Section::Page(c) => &c.name,
            Section::Group(g) => &g.title,
            Section::Stations => "Radio Stations (from your top artists)",
            Section::Recent => "Recently Played",
        }
    }
}

fn browse_entry(item: Item) -> Entry {
    match item {
        Item::Playlist(p) => Entry::Playlist(p),
        Item::Album(a) => Entry::Album(a),
        Item::Page(c) => Entry::Section(Section::Page(c)),
    }
}

#[derive(Clone)]
pub enum Entry {
    Track(Track),
    Album(Album),
    Artist(Artist),
    Playlist(Playlist),
    Section(Section),
    /// Radio seeded by an artist or track URI.
    Station { name: String, seed: String },
}

pub struct SearchState {
    pub query: String,
    pub editing: bool,
    pub seq: u64,
}

pub struct EntryList {
    pub title: String,
    pub entries: Vec<Entry>,
    pub state: ListState,
    /// Render-only state: `state` shifted past group header rows, carrying the scroll offset.
    pub view: TableState,
    pub search: Option<SearchState>,
    pub loading: bool,
    /// Load id for asynchronously filled lists (0 = not applicable).
    pub load: u64,
}

impl EntryList {
    fn new(title: impl Into<String>, entries: Vec<Entry>, loading: bool, load: u64) -> Self {
        let mut state = ListState::default();
        if !entries.is_empty() {
            state.select(Some(0));
        }
        Self { title: title.into(), entries, state, view: TableState::default(), search: None, loading, load }
    }
}

pub enum View {
    Tracks(TrackList),
    Entries(EntryList),
}

/// Sidebar rows before the user's playlists start.
pub const FIXED_SIDEBAR_ITEMS: usize = 5;

pub enum SidebarItem {
    Search,
    Browse,
    Liked,
    Albums,
    RecentPlaylists,
    Playlist(Playlist),
}

impl SidebarItem {
    pub fn label(&self) -> &str {
        match self {
            SidebarItem::Search => "Search",
            SidebarItem::Browse => "Browse",
            SidebarItem::Liked => "Liked Songs",
            SidebarItem::Albums => "Saved Albums",
            SidebarItem::RecentPlaylists => "Recent Playlists",
            SidebarItem::Playlist(p) => &p.name,
        }
    }

    /// Stable identity across playlist reloads, which may reorder the sidebar.
    fn key(&self) -> &str {
        match self {
            SidebarItem::Playlist(p) => &p.uri,
            other => other.label(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Content,
}

// ---------------------------------------------------------------- playback

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Stopped,
    Loading,
    Playing,
    Paused,
}

pub struct Now {
    pub track: Option<TrackInfo>,
    pub status: Status,
    base_ms: u32,
    since: Instant,
    pub volume: u16,
    pub shuffle: bool,
    pub repeat_ctx: bool,
    pub repeat_track: bool,
}

/// Album art for the current track; `proto` is filled once the download decodes.
pub struct Cover {
    pub url: String,
    pub proto: Option<StatefulProtocol>,
}

impl Now {
    fn set_position(&mut self, ms: u32) {
        self.base_ms = ms;
        self.since = Instant::now();
    }

    /// Position interpolated from the last player event.
    pub fn position_ms(&self) -> u32 {
        let mut p = self.base_ms;
        if self.status == Status::Playing {
            p = p.saturating_add(self.since.elapsed().as_millis() as u32);
        }
        match &self.track {
            Some(t) => p.min(t.duration_ms),
            None => p,
        }
    }

    pub fn volume_percent(&self) -> u16 {
        (u32::from(self.volume) * 100 / u32::from(u16::MAX)) as u16
    }
}

// ---------------------------------------------------------------- app

pub struct App {
    api: Api,
    player: PlayerHandle,
    cache: Cache,
    tx: UnboundedSender<Event>,

    pub sidebar: Vec<SidebarItem>,
    pub sidebar_state: ListState,
    /// Render-only state: `sidebar_state` shifted past the separator row, carrying the scroll offset.
    pub sidebar_view: ListState,
    pub focus: Focus,
    pub view: Option<View>,
    back: Vec<View>,
    /// Key of the sidebar item the current view stack was opened from.
    view_root: Option<String>,
    pub now: Now,
    pub status: Option<(String, Instant)>,
    pub help: bool,
    /// First visible help line; clamped by the renderer when the overlay fits.
    pub help_scroll: u16,
    pub settings: Settings,
    pub fullscreen: bool,
    /// The full-screen player's Up Next pane is showing (and has focus).
    pub up_next_open: bool,
    pub up_next: TrackList,
    pub picker: Picker,
    pub cover: Option<Cover>,
    /// Rows visible in the content pane; set by the renderer, used for paging.
    pub page: usize,
    /// Renderer-owned pane transition state.
    pub anim: crate::ui::Anim,

    next_load: u64,
    next_search: u64,
    pub quit: bool,
    pub dirty: bool,
}

impl App {
    pub fn new(
        api: Api,
        player: PlayerHandle,
        cache: Cache,
        tx: UnboundedSender<Event>,
        volume: u16,
        picker: Picker,
        settings: Settings,
    ) -> Self {
        let mut app = Self {
            api,
            player,
            cache,
            tx,
            sidebar: vec![
                SidebarItem::Search,
                SidebarItem::Browse,
                SidebarItem::Liked,
                SidebarItem::Albums,
                SidebarItem::RecentPlaylists,
            ],
            sidebar_state: ListState::default().with_selected(Some(2)),
            sidebar_view: ListState::default(),
            focus: Focus::Sidebar,
            view: None,
            back: Vec::new(),
            view_root: None,
            now: Now {
                track: None,
                status: Status::Stopped,
                base_ms: 0,
                since: Instant::now(),
                volume,
                shuffle: false,
                repeat_ctx: false,
                repeat_track: false,
            },
            status: None,
            help: false,
            help_scroll: 0,
            settings,
            fullscreen: false,
            up_next_open: false,
            up_next: TrackList::new("Up Next".into(), 0, None, None),
            picker,
            cover: None,
            page: 10,
            anim: crate::ui::Anim::default(),
            next_load: 0,
            next_search: 0,
            quit: false,
            dirty: true,
        };
        app.spawn_playlists();
        app.open_sidebar_selection();
        app
    }

    /// Views stacked behind the current one; grows on forward navigation.
    pub fn depth(&self) -> usize {
        self.back.len()
    }

    fn toast(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        tracing::warn!("{msg}");
        self.status = Some((msg, Instant::now()));
        self.dirty = true;
    }

    /// Track the cover for the current track, downloading it if it changed.
    fn set_cover(&mut self, url: Option<String>) {
        if self.cover.as_ref().map(|c| &c.url) == url.as_ref() {
            return;
        }
        self.cover = url.clone().map(|url| Cover { url, proto: None });
        let Some(url) = url else { return };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let image = async {
                let bytes = reqwest::get(&url).await?.error_for_status()?.bytes().await?;
                let img = tokio::task::spawn_blocking(move || image::load_from_memory(&bytes)).await??;
                anyhow::Ok(img)
            }
            .await
            .map_err(|e| tracing::warn!("cover {url}: {e:#}"))
            .ok();
            let _ = tx.send(Event::Data(Data::Cover { url, image }));
        });
    }

    // ------------------------------------------------------------ events

    pub fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Key(k) => self.on_key(k),
            Event::Resize => self.dirty = true,
            Event::Tick => {
                if self.now.status == Status::Playing {
                    self.dirty = true;
                }
                if matches!(&self.status, Some((_, t)) if t.elapsed() > STATUS_TTL) {
                    self.status = None;
                    self.dirty = true;
                }
            }
            Event::Playback(p) => self.on_playback(p),
            Event::Data(d) => self.on_data(d),
        }
    }

    fn on_playback(&mut self, ev: PlaybackEvent) {
        self.dirty = true;
        let now = &mut self.now;
        match ev {
            PlaybackEvent::Loading => now.status = Status::Loading,
            PlaybackEvent::Track(t) => {
                let url = t.cover_url.clone();
                now.track = Some(t);
                now.set_position(0);
                self.set_cover(url);
                self.refresh_up_next(QUEUE_SETTLE);
            }
            PlaybackEvent::Playing { position_ms } => {
                now.status = Status::Playing;
                now.set_position(position_ms);
            }
            PlaybackEvent::Paused { position_ms } => {
                now.status = Status::Paused;
                now.set_position(position_ms);
            }
            PlaybackEvent::Stopped => {
                now.status = Status::Stopped;
                now.set_position(0);
            }
            PlaybackEvent::Seeked { position_ms } => now.set_position(position_ms),
            PlaybackEvent::Volume(v) => now.volume = v,
            PlaybackEvent::Shuffle(s) => {
                now.shuffle = s;
                self.refresh_up_next(QUEUE_SETTLE);
            }
            PlaybackEvent::Repeat { context, track } => {
                now.repeat_ctx = context;
                now.repeat_track = track;
            }
            PlaybackEvent::Unavailable => self.toast("Track unavailable"),
        }
    }

    fn on_data(&mut self, d: Data) {
        self.dirty = true;
        match d {
            Data::Cover { url, image } => {
                if let (Some(c), Some(img)) = (&mut self.cover, image)
                    && c.url == url
                {
                    c.proto = Some(self.picker.new_resize_protocol(img));
                }
            }
            Data::Playlists(Ok(list)) => {
                self.sidebar.truncate(FIXED_SIDEBAR_ITEMS);
                self.sidebar.extend(list.into_iter().map(SidebarItem::Playlist));
            }
            Data::Playlists(Err(e)) => self.toast(format!("Playlists: {e:#}")),
            Data::Albums(res) => {
                if let Some(View::Entries(l)) = &mut self.view
                    && l.title == "Saved Albums"
                {
                    l.loading = false;
                    match res {
                        Ok(albums) => {
                            l.entries = albums.into_iter().map(Entry::Album).collect();
                            if !l.entries.is_empty() {
                                l.state.select(Some(0));
                            }
                        }
                        Err(e) => self.toast(format!("Albums: {e:#}")),
                    }
                }
            }
            Data::CachedTracks { load, cache, fresh } => {
                if let Some(l) = self.track_list_mut(load) {
                    // Only paint cache into an untouched list; live data may have won the race.
                    if l.tracks.is_empty() {
                        l.tracks = cache.tracks;
                        l.total = cache.total;
                        l.skipped = cache.skipped;
                        l.from_cache = !fresh;
                        if !l.tracks.is_empty() {
                            l.state.select(Some(0));
                        }
                    }
                    if fresh {
                        l.loading = false;
                    }
                }
            }
            Data::TrackChunk { load, tracks, total } => {
                if let Some(l) = self.track_list_mut(load) {
                    if l.from_cache {
                        // First live data replaces the cached copy; keep the cursor where it was.
                        l.from_cache = false;
                        l.tracks.clear();
                        l.skipped = 0;
                    }
                    l.total = total;
                    let before = l.tracks.len();
                    let n = tracks.len();
                    l.tracks.extend(tracks.into_iter().filter(Track::is_playable_track));
                    l.skipped += (n - (l.tracks.len() - before)) as u32;
                    if l.state.selected().is_none() && !l.tracks.is_empty() {
                        l.state.select(Some(0));
                    }
                }
            }
            Data::TrackLoadDone { load, error } => {
                let mut to_cache = None;
                if let Some(l) = self.track_list_mut(load) {
                    l.loading = false;
                    if error.is_some() && l.from_cache {
                        // Live load failed; the cached copy stays on screen.
                        l.from_cache = false;
                    } else if error.is_none() {
                        to_cache = l.cache_key.clone().map(|k| {
                            (k, TrackCache { tracks: l.tracks.clone(), total: l.total, skipped: l.skipped })
                        });
                    }
                }
                if let Some((key, data)) = to_cache {
                    let cache = self.cache.clone();
                    tokio::spawn(async move { cache.write(&key, data).await });
                }
                if let Some(e) = error {
                    self.toast(format!("Load failed: {e}"));
                }
            }
            Data::Entries { load, result } => {
                if let Some(l) = self.entry_list_mut(load) {
                    l.loading = false;
                    if let Ok(entries) = &result {
                        l.entries = entries.clone();
                        l.state.select(if l.entries.is_empty() { None } else { Some(0) });
                    }
                }
                if let Err(e) = result {
                    self.toast(format!("Load failed: {e:#}"));
                }
            }
            Data::Station { name, result } => match result {
                Ok(uris) => {
                    self.player.play_tracks(uris, 0);
                    self.toast(format!("Playing {name} radio"));
                }
                Err(e) => self.toast(format!("Radio: {e:#}")),
            },
            Data::Queue { seq, result } => {
                let l = &mut self.up_next;
                if l.load == seq {
                    l.loading = false;
                    match result {
                        Ok(tracks) => {
                            l.total = tracks.len() as u32;
                            l.tracks = tracks;
                            let sel = l.state.selected().unwrap_or(0).min(l.tracks.len().saturating_sub(1));
                            l.state.select((!l.tracks.is_empty()).then_some(sel));
                        }
                        Err(e) => self.toast(format!("Up next: {e:#}")),
                    }
                }
            }
            Data::Search { seq, result } => {
                if let Some(View::Entries(l)) = &mut self.view
                    && matches!(&l.search, Some(s) if s.seq == seq)
                {
                    l.loading = false;
                    match result {
                        Ok(r) => {
                            l.entries = flatten_search(r);
                            l.state.select(if l.entries.is_empty() { None } else { Some(0) });
                        }
                        Err(e) => self.toast(format!("Search: {e:#}")),
                    }
                }
            }
        }
    }

    /// Find the track list (current view or navigation stack) for a load id.
    fn track_list_mut(&mut self, load: u64) -> Option<&mut TrackList> {
        self.view.iter_mut().chain(self.back.iter_mut()).find_map(|v| match v {
            View::Tracks(l) if l.load == load => Some(l),
            _ => None,
        })
    }

    fn entry_list_mut(&mut self, load: u64) -> Option<&mut EntryList> {
        self.view.iter_mut().chain(self.back.iter_mut()).find_map(|v| match v {
            View::Entries(l) if l.load == load => Some(l),
            _ => None,
        })
    }

    // ------------------------------------------------------------ keys

    fn on_key(&mut self, k: KeyEvent) {
        self.dirty = true;
        if self.help {
            // Short terminals can't show all of it: j/k scroll, anything else closes.
            match k.code {
                KeyCode::Char('j') | KeyCode::Down => self.help_scroll = self.help_scroll.saturating_add(1),
                KeyCode::Char('k') | KeyCode::Up => self.help_scroll = self.help_scroll.saturating_sub(1),
                _ => {
                    self.help = false;
                    self.help_scroll = 0;
                }
            }
            return;
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.settings.open {
            if let Outcome::Close = self.settings.on_key(k) {
                self.settings.open = false;
            }
            return;
        }
        if self.editing_search() {
            self.on_search_key(k);
            return;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        // The full-screen player answers transport keys, ← → for its Up Next pane, and list
        // movement and ⏎ while that pane is open; everything else would act on hidden panes.
        if self.fullscreen {
            let open = self.up_next_open;
            match k.code {
                KeyCode::Right | KeyCode::Char('l') => return self.open_up_next(),
                KeyCode::Left | KeyCode::Char('h') => return self.up_next_open = false,
                KeyCode::Esc | KeyCode::Backspace if open => return self.up_next_open = false,
                KeyCode::Char('j' | 'k' | 'g' | 'G')
                | KeyCode::Down
                | KeyCode::Up
                | KeyCode::PageDown
                | KeyCode::PageUp
                | KeyCode::Home
                | KeyCode::End
                    if open => {}
                KeyCode::Char('d' | 'u') if open && ctrl => {}
                KeyCode::Enter if open => return self.play_up_next(),
                KeyCode::Char('q' | ' ' | 'n' | 'p' | 's' | 'r' | '+' | '=' | '-' | '>' | '<' | '?' | 'f' | ',')
                | KeyCode::Esc
                | KeyCode::Backspace => {}
                _ => return,
            }
        }
        match k.code {
            KeyCode::Char('f') => {
                self.fullscreen = !self.fullscreen;
                self.up_next_open = false;
            }
            KeyCode::Esc | KeyCode::Backspace if self.fullscreen => self.fullscreen = false,
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char(',') => self.settings.open = true,
            KeyCode::Char(' ') => self.player.play_pause(),
            KeyCode::Char('n') => self.player.next(),
            KeyCode::Char('p') => self.player.prev(),
            KeyCode::Char('s') => self.player.shuffle(!self.now.shuffle),
            KeyCode::Char('r') => self.player.repeat(self.now.repeat_ctx, self.now.repeat_track),
            KeyCode::Char('R') => self.start_radio(),
            KeyCode::Char('+' | '=') => self.change_volume(true),
            KeyCode::Char('-') => self.change_volume(false),
            KeyCode::Char('>') => self.seek(true),
            KeyCode::Char('<') => self.seek(false),
            KeyCode::Char('/') => self.open_search(),
            KeyCode::Tab if self.focus == Focus::Content => self.focus = Focus::Sidebar,
            KeyCode::Tab | KeyCode::Char('l') | KeyCode::Right => self.enter_content(),
            KeyCode::Char('h') | KeyCode::Left => self.focus = Focus::Sidebar,
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('d') if ctrl => self.move_selection((self.page / 2).max(1) as isize),
            KeyCode::Char('u') if ctrl => self.move_selection(-((self.page / 2).max(1) as isize)),
            KeyCode::PageDown => self.move_selection(self.page as isize),
            KeyCode::PageUp => self.move_selection(-(self.page as isize)),
            KeyCode::Char('g') | KeyCode::Home => self.move_selection(isize::MIN),
            KeyCode::Char('G') | KeyCode::End => self.move_selection(isize::MAX),
            KeyCode::Enter => match self.focus {
                Focus::Sidebar => {
                    self.open_sidebar_selection();
                    if self.view.is_some() {
                        self.focus_content();
                    }
                }
                Focus::Content => self.activate(),
            },
            KeyCode::Esc | KeyCode::Backspace => self.go_back(),
            _ => {}
        }
    }

    /// Moves into the content pane for the highlighted sidebar item. The existing
    /// view stack (including drill-downs) is kept only if it belongs to that item;
    /// otherwise the item is opened fresh.
    fn enter_content(&mut self) {
        if self.focus == Focus::Sidebar {
            let highlighted = self.sidebar_state.selected().and_then(|i| self.sidebar.get(i)).map(SidebarItem::key);
            if highlighted != self.view_root.as_deref() {
                self.open_sidebar_selection();
            }
        }
        if self.view.is_some() {
            self.focus_content();
        }
    }

    /// Moves focus to the content pane; an empty search view starts in typing mode
    /// so the query can be entered without pressing `/` first.
    fn focus_content(&mut self) {
        self.focus = Focus::Content;
        if let Some(View::Entries(EntryList { search: Some(s), .. })) = &mut self.view
            && s.query.is_empty()
        {
            s.editing = true;
        }
    }

    fn editing_search(&self) -> bool {
        matches!(&self.view, Some(View::Entries(EntryList { search: Some(s), .. })) if s.editing)
    }

    fn on_search_key(&mut self, k: KeyEvent) {
        let Some(View::Entries(l)) = &mut self.view else { return };
        let Some(s) = &mut l.search else { return };
        match k.code {
            KeyCode::Esc => s.editing = false,
            KeyCode::Backspace => {
                s.query.pop();
            }
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => s.query.push(c),
            KeyCode::Enter => {
                s.editing = false;
                let q = s.query.trim().to_string();
                if q.is_empty() {
                    return;
                }
                self.next_search += 1;
                s.seq = self.next_search;
                let seq = s.seq;
                l.loading = true;
                let (api, tx) = (self.api.clone(), self.tx.clone());
                tokio::spawn(async move {
                    let result = api.search(&q).await;
                    let _ = tx.send(Event::Data(Data::Search { seq, result }));
                });
                self.focus = Focus::Content;
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ playback controls

    fn change_volume(&mut self, up: bool) {
        let v = if up {
            self.now.volume.saturating_add(VOLUME_STEP)
        } else {
            self.now.volume.saturating_sub(VOLUME_STEP)
        };
        self.now.volume = v;
        self.player.set_volume(v);
    }

    fn seek(&mut self, forward: bool) {
        let Some(t) = &self.now.track else { return };
        let pos = self.now.position_ms();
        let target = if forward {
            pos.saturating_add(SEEK_STEP_MS).min(t.duration_ms)
        } else {
            pos.saturating_sub(SEEK_STEP_MS)
        };
        self.now.set_position(target);
        self.player.seek(target);
    }

    // ------------------------------------------------------------ navigation

    /// Shows the full-screen player's Up Next pane with a fresh copy of the queue.
    fn open_up_next(&mut self) {
        self.up_next_open = true;
        self.refresh_up_next(Duration::ZERO);
    }

    /// Fetches the queue after `delay` if the Up Next pane is showing; an earlier request still
    /// in flight is superseded.
    fn refresh_up_next(&mut self, delay: Duration) {
        if !(self.fullscreen && self.up_next_open) {
            return;
        }
        let seq = self.new_load();
        let l = &mut self.up_next;
        l.load = seq;
        // A refresh keeps the current rows up; only an empty pane shows "Loading…".
        l.loading = l.tracks.is_empty();
        let (api, tx) = (self.api.clone(), self.tx.clone());
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let result = api.queue().await;
            let _ = tx.send(Event::Data(Data::Queue { seq, result }));
        });
    }

    /// Jumps to the selected Up Next track by skipping everything before it, so the rest of the
    /// queue and context keep playing after it.
    fn play_up_next(&mut self) {
        let l = &mut self.up_next;
        if l.loading {
            return;
        }
        let Some(i) = l.state.selected().filter(|&i| i < l.tracks.len()) else { return };
        self.player.skip(i + 1);
        // The pane refreshes on the track change; the new first row is what follows it.
        l.state.select(Some(0));
    }

    fn move_selection(&mut self, delta: isize) {
        let (len, sel): (usize, Option<usize>) = match self.focus {
            _ if self.fullscreen => (self.up_next.tracks.len(), self.up_next.state.selected()),
            Focus::Sidebar => (self.sidebar.len(), self.sidebar_state.selected()),
            Focus::Content => match &self.view {
                Some(View::Tracks(l)) => (l.tracks.len(), l.state.selected()),
                Some(View::Entries(l)) => (l.entries.len(), l.state.selected()),
                None => return,
            },
        };
        if len == 0 {
            return;
        }
        let next = match delta {
            isize::MIN => 0,
            isize::MAX => len - 1,
            d => (sel.unwrap_or(0) as isize + d).clamp(0, len as isize - 1) as usize,
        };
        match self.focus {
            _ if self.fullscreen => self.up_next.state.select(Some(next)),
            Focus::Sidebar => self.sidebar_state.select(Some(next)),
            Focus::Content => match &mut self.view {
                Some(View::Tracks(l)) => l.state.select(Some(next)),
                Some(View::Entries(l)) => l.state.select(Some(next)),
                None => {}
            },
        }
    }

    fn push_view(&mut self, v: View) {
        if let Some(old) = self.view.replace(v) {
            self.back.push(old);
        }
    }

    fn go_back(&mut self) {
        if self.focus == Focus::Sidebar {
            return;
        }
        if let Some(prev) = self.back.pop() {
            self.view = Some(prev);
        } else {
            self.focus = Focus::Sidebar;
        }
    }

    fn open_search(&mut self) {
        let idx = self.sidebar.iter().position(|s| matches!(s, SidebarItem::Search)).unwrap_or(0);
        self.sidebar_state.select(Some(idx));
        self.open_sidebar_selection();
        self.focus_content();
    }

    /// Sidebar selections replace the whole view stack (they are roots).
    fn open_sidebar_selection(&mut self) {
        let Some(i) = self.sidebar_state.selected() else { return };
        self.back.clear();
        self.view = None;
        self.view_root = Some(self.sidebar[i].key().to_owned());
        match &self.sidebar[i] {
            SidebarItem::Search => {
                let mut list = EntryList::new("Search", Vec::new(), false, 0);
                list.search = Some(SearchState { query: String::new(), editing: false, seq: 0 });
                self.view = Some(View::Entries(list));
            }
            SidebarItem::Browse => {
                // Stations and Recent stay reachable if the live catalogue fails to load.
                let fixed = [Entry::Section(Section::Stations), Entry::Section(Section::Recent)];
                let load = self.new_load();
                self.view = Some(View::Entries(EntryList::new("Browse", fixed.to_vec(), true, load)));
                let session = self.player.session();
                self.spawn_entries(
                    load,
                    "browse-hubs".into(),
                    CATEGORY_TTL,
                    async move { browse::categories(&session).await },
                    |cats: Vec<Category>| {
                        [Entry::Section(Section::Stations), Entry::Section(Section::Recent)]
                            .into_iter()
                            .chain(cats.into_iter().map(|c| Entry::Section(Section::Page(c))))
                            .collect()
                    },
                );
            }
            SidebarItem::Liked => {
                let load = self.new_load();
                self.view = Some(View::Tracks(TrackList::new("Liked Songs".into(), load, None, Some("liked".into()))));
                let api = self.api.clone();
                self.spawn_tracks(load, move |on| async move { api.saved_tracks(on).await });
            }
            SidebarItem::Albums => {
                self.view = Some(View::Entries(EntryList::new("Saved Albums", Vec::new(), true, 0)));
                let (api, tx) = (self.api.clone(), self.tx.clone());
                tokio::spawn(async move {
                    let _ = tx.send(Event::Data(Data::Albums(api.saved_albums().await)));
                });
            }
            SidebarItem::RecentPlaylists => {
                let load = self.new_load();
                self.view = Some(View::Entries(EntryList::new("Recent Playlists", Vec::new(), true, load)));
                let known: Vec<Playlist> = self
                    .sidebar
                    .iter()
                    .filter_map(|s| if let SidebarItem::Playlist(p) = s { Some(p.clone()) } else { None })
                    .collect();
                let (api, session, tx) = (self.api.clone(), self.player.session(), self.tx.clone());
                tokio::spawn(async move {
                    let result = async {
                        let uris = api.recent_playlist_uris().await?;
                        let known = &known;
                        let session = &session;
                        let lookups = uris.into_iter().map(|uri| async move {
                            if let Some(p) = known.iter().find(|p| p.uri == uri) {
                                return Some(p.clone());
                            }
                            match crate::player::playlist_info(session, &uri).await {
                                Ok(p) => {
                                    tracing::info!("recent playlists: {uri} resolved ({})", p.name);
                                    Some(p)
                                }
                                Err(e) => {
                                    tracing::warn!("recent playlists: {uri} unresolved, dropping: {e:#}");
                                    None
                                }
                            }
                        });
                        anyhow::Ok(
                            futures::future::join_all(lookups).await.into_iter().flatten().map(Entry::Playlist).collect(),
                        )
                    }
                    .await;
                    let _ = tx.send(Event::Data(Data::Entries { load, result }));
                });
            }
            SidebarItem::Playlist(p) => {
                let (id, uri, name) = (p.id.clone(), p.uri.clone(), p.name.clone());
                self.open_playlist(id, uri, name, false);
            }
        }
    }

    fn open_section(&mut self, section: Section) {
        if let Section::Recent = section {
            // Always live: recency is the whole point.
            let load = self.new_load();
            self.push_view(View::Tracks(TrackList::new(section.label().into(), load, None, None)));
            let api = self.api.clone();
            self.spawn_tracks(load, move |on| async move { api.recently_played(on).await });
            return;
        }
        let load = self.new_load();
        self.push_view(View::Entries(EntryList::new(section.label(), Vec::new(), true, load)));
        let session = self.player.session();
        match section {
            Section::Page(c) => self.spawn_entries(
                load,
                format!("browse-page-{}", c.uri),
                CATEGORY_TTL,
                async move { browse::page_groups(&session, &c.uri).await },
                |groups: Vec<Group>| groups.into_iter().map(|g| Entry::Section(Section::Group(g))).collect(),
            ),
            Section::Group(g) => self.spawn_entries(
                load,
                format!("browse-section-{}", g.uri),
                SECTION_TTL,
                async move { browse::group_items(&session, g).await },
                |items: Vec<Item>| items.into_iter().map(browse_entry).collect(),
            ),
            Section::Stations => {
                let api = self.api.clone();
                self.spawn_entries(
                    load,
                    "top-artists".into(),
                    TOP_ARTISTS_TTL,
                    async move { api.top_artists().await },
                    |items: Vec<Artist>| {
                        items.into_iter().map(|a| Entry::Station { name: a.name, seed: a.uri }).collect()
                    },
                );
            }
            Section::Recent => {}
        }
    }

    /// Fill an entry list from the disk cache when fresh; otherwise fetch, then cache. If the
    /// fetch fails, an expired cache entry is better than an error page.
    fn spawn_entries<T, Fut>(
        &self,
        load: u64,
        key: String,
        ttl: Duration,
        fetch: Fut,
        wrap: fn(Vec<T>) -> Vec<Entry>,
    ) where
        T: Serialize + DeserializeOwned + Clone + Send + 'static,
        Fut: Future<Output = anyhow::Result<Vec<T>>> + Send + 'static,
    {
        let (cache, tx) = (self.cache.clone(), self.tx.clone());
        tokio::spawn(async move {
            let cached = cache.read::<Vec<T>>(&key).await;
            if let Some(c) = &cached
                && c.age < ttl
            {
                let _ = tx.send(Event::Data(Data::Entries { load, result: Ok(wrap(c.data.clone())) }));
                return;
            }
            let result = match fetch.await {
                Ok(items) => {
                    cache.write(&key, items.clone()).await;
                    Ok(wrap(items))
                }
                Err(e) => match cached {
                    Some(c) => {
                        tracing::warn!("refresh of {key} failed, using stale cache: {e:#}");
                        Ok(wrap(c.data))
                    }
                    None => Err(e),
                },
            };
            let _ = tx.send(Event::Data(Data::Entries { load, result }));
        });
    }

    fn open_playlist(&mut self, id: String, uri: String, name: String, push: bool) {
        let load = self.new_load();
        let list = View::Tracks(TrackList::new(name, load, Some(uri.clone()), Some(format!("playlist-{id}"))));
        if push {
            self.push_view(list);
        } else {
            self.view = Some(list);
        }
        let (api, player) = (self.api.clone(), self.player.clone());
        self.spawn_tracks(load, move |mut on| async move {
            match api.playlist_tracks(&id, &mut on).await {
                // Web API only serves playlists you own; use the streaming session for the rest.
                Err(e) if HttpError::is_inaccessible(&e) => player.playlist_tracks(&uri, on).await,
                res => res,
            }
        });
    }

    fn open_album(&mut self, a: &Album) {
        let load = self.new_load();
        self.push_view(View::Tracks(TrackList::new(
            a.name.clone(),
            load,
            Some(a.uri.clone()),
            Some(format!("album-{}", a.id)),
        )));
        let (api, id, name) = (self.api.clone(), a.id.clone(), a.name.clone());
        self.spawn_tracks(load, move |on| async move { api.album_tracks(&id, &name, on).await });
    }

    fn new_load(&mut self) -> u64 {
        self.next_load += 1;
        self.next_load
    }

    /// Show cached playlists immediately, then refresh them.
    fn spawn_playlists(&self) {
        const KEY: &str = "my-playlists";
        let (api, tx, cache) = (self.api.clone(), self.tx.clone(), self.cache.clone());
        tokio::spawn(async move {
            let cached = cache.read::<Vec<Playlist>>(KEY).await;
            let had_cache = cached.is_some();
            if let Some(c) = cached {
                let _ = tx.send(Event::Data(Data::Playlists(Ok(c.data))));
            }
            match api.my_playlists().await {
                Ok(list) => {
                    cache.write(KEY, list.clone()).await;
                    let _ = tx.send(Event::Data(Data::Playlists(Ok(list))));
                }
                // A failed refresh only matters if there was nothing to show.
                Err(e) if !had_cache => {
                    let _ = tx.send(Event::Data(Data::Playlists(Err(e))));
                }
                Err(e) => tracing::warn!("playlist refresh failed: {e:#}"),
            }
        });
    }

    /// Load a track list: paint the cached copy first (skipping the network if it is fresh),
    /// otherwise stream live pages tagged with `load`.
    fn spawn_tracks<F, Fut>(&self, load: u64, f: F)
    where
        F: FnOnce(Box<dyn FnMut(Vec<Track>, u32) + Send>) -> Fut + Send + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send,
    {
        let key = match self.view.as_ref() {
            Some(View::Tracks(l)) if l.load == load => l.cache_key.clone(),
            _ => self.back.iter().find_map(|v| match v {
                View::Tracks(l) if l.load == load => l.cache_key.clone(),
                _ => None,
            }),
        };
        let (tx, cache) = (self.tx.clone(), self.cache.clone());
        tokio::spawn(async move {
            if let Some(key) = &key
                && let Some(c) = cache.read::<TrackCache>(key).await
            {
                let fresh = c.age < TRACKS_FRESH;
                let _ = tx.send(Event::Data(Data::CachedTracks { load, cache: c.data, fresh }));
                if fresh {
                    return;
                }
            }
            let chunk_tx = tx.clone();
            let res = f(Box::new(move |tracks, total| {
                let _ = chunk_tx.send(Event::Data(Data::TrackChunk { load, tracks, total }));
            }))
            .await;
            let error = res.err().map(|e| format!("{e:#}"));
            let _ = tx.send(Event::Data(Data::TrackLoadDone { load, error }));
        });
    }

    // ------------------------------------------------------------ activation

    fn activate(&mut self) {
        match &self.view {
            Some(View::Tracks(l)) => {
                let Some(i) = l.state.selected() else { return };
                // The context is only usable when every item is present, so indices line up.
                let exact = l.skipped == 0 && !l.loading && l.tracks.len() as u32 == l.total;
                if l.context.is_some() && !exact {
                    tracing::info!(
                        "playing {} from bare track URIs, not context (skipped {}, loading {}, {}/{} loaded)",
                        l.title, l.skipped, l.loading, l.tracks.len(), l.total
                    );
                }
                match (&l.context, exact) {
                    (Some(uri), true) => self.player.play_context(uri.clone(), i),
                    _ => {
                        let uris = l.tracks.iter().map(|t| t.uri.clone()).collect();
                        self.player.play_tracks(uris, i);
                    }
                }
            }
            Some(View::Entries(l)) => {
                let Some(entry) = l.state.selected().and_then(|i| l.entries.get(i)).cloned() else { return };
                match entry {
                    Entry::Track(t) => {
                        // Play the selected track followed by the other track results.
                        let mut uris = vec![t.uri.clone()];
                        uris.extend(l.entries.iter().filter_map(|e| match e {
                            Entry::Track(o) if o.uri != t.uri => Some(o.uri.clone()),
                            _ => None,
                        }));
                        self.player.play_tracks(uris, 0);
                    }
                    Entry::Album(a) => self.open_album(&a),
                    Entry::Playlist(p) => self.open_playlist(p.id, p.uri, p.name, true),
                    Entry::Artist(a) => {
                        self.player.play_context(a.uri, 0);
                        self.toast(format!("Playing {}", a.name));
                    }
                    Entry::Section(s) => self.open_section(s),
                    Entry::Station { name, seed } => self.play_station(name, seed),
                }
            }
            None => {}
        }
    }

    /// Radio from the selected track/artist/station (`R`).
    fn start_radio(&mut self) {
        if self.focus != Focus::Content {
            return;
        }
        let seed = match &self.view {
            Some(View::Tracks(l)) => l
                .state
                .selected()
                .and_then(|i| l.tracks.get(i))
                .map(|t| (t.name.clone(), t.uri.clone())),
            Some(View::Entries(l)) => match l.state.selected().and_then(|i| l.entries.get(i)) {
                Some(Entry::Track(t)) => Some((t.name.clone(), t.uri.clone())),
                Some(Entry::Artist(a)) => Some((a.name.clone(), a.uri.clone())),
                Some(Entry::Station { name, seed }) => Some((name.clone(), seed.clone())),
                _ => None,
            },
            None => None,
        };
        match seed {
            Some((name, uri)) => self.play_station(name, uri),
            None => self.toast("Nothing here to start a radio from"),
        }
    }

    fn play_station(&mut self, name: String, seed: String) {
        self.toast(format!("Starting {name} radio…"));
        let (session, tx) = (self.player.session(), self.tx.clone());
        tokio::spawn(async move {
            let result = browse::station_tracks(&session, &seed).await;
            let _ = tx.send(Event::Data(Data::Station { name, result }));
        });
    }
}

fn flatten_search(r: SearchResults) -> Vec<Entry> {
    fn items<T>(p: Option<crate::api::models::Page<Option<T>>>) -> impl Iterator<Item = T> {
        p.into_iter().flat_map(|p| p.items).flatten()
    }
    items(r.tracks)
        .filter(Track::is_playable_track)
        .map(Entry::Track)
        .chain(items(r.albums).map(Entry::Album))
        .chain(items(r.artists).map(Entry::Artist))
        .chain(items(r.playlists).map(Entry::Playlist))
        .collect()
}
