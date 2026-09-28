use crate::api::{
    HttpError,
    Api,
    models::{Album, Artist, Playlist, SearchResults, Track},
};
use crate::event::{Data, Event};
use crate::player::{PlaybackEvent, PlayerHandle, TrackInfo};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::{ListState, TableState};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

const VOLUME_STEP: u16 = u16::MAX / 20;
const SEEK_STEP_MS: u32 = 5_000;
const STATUS_TTL: Duration = Duration::from_secs(5);

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
}

impl TrackList {
    fn new(title: String, load: u64, context: Option<String>) -> Self {
        Self {
            title,
            tracks: Vec::new(),
            total: 0,
            skipped: 0,
            loading: true,
            load,
            context,
            state: TableState::default(),
        }
    }
}

#[derive(Clone)]
pub enum Entry {
    Track(Track),
    Album(Album),
    Artist(Artist),
    Playlist(Playlist),
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
    pub search: Option<SearchState>,
    pub loading: bool,
}

pub enum View {
    Tracks(TrackList),
    Entries(EntryList),
}

pub enum SidebarItem {
    Search,
    Liked,
    Albums,
    Playlist(Playlist),
}

impl SidebarItem {
    pub fn label(&self) -> &str {
        match self {
            SidebarItem::Search => "Search",
            SidebarItem::Liked => "Liked Songs",
            SidebarItem::Albums => "Saved Albums",
            SidebarItem::Playlist(p) => &p.name,
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
    tx: UnboundedSender<Event>,

    pub sidebar: Vec<SidebarItem>,
    pub sidebar_state: ListState,
    pub focus: Focus,
    pub view: Option<View>,
    back: Vec<View>,
    pub now: Now,
    pub status: Option<(String, Instant)>,
    pub help: bool,
    /// Rows visible in the content pane; set by the renderer, used for paging.
    pub page: usize,

    next_load: u64,
    next_search: u64,
    pub quit: bool,
    pub dirty: bool,
}

impl App {
    pub fn new(api: Api, player: PlayerHandle, tx: UnboundedSender<Event>, volume: u16) -> Self {
        let mut app = Self {
            api,
            player,
            tx,
            sidebar: vec![SidebarItem::Search, SidebarItem::Liked, SidebarItem::Albums],
            sidebar_state: ListState::default().with_selected(Some(1)),
            focus: Focus::Sidebar,
            view: None,
            back: Vec::new(),
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
            page: 10,
            next_load: 0,
            next_search: 0,
            quit: false,
            dirty: true,
        };
        app.spawn_playlists();
        app.open_sidebar_selection();
        app
    }

    fn toast(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        tracing::warn!("{msg}");
        self.status = Some((msg, Instant::now()));
        self.dirty = true;
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
                now.track = Some(t);
                now.set_position(0);
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
            PlaybackEvent::Shuffle(s) => now.shuffle = s,
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
            Data::Playlists(Ok(list)) => {
                self.sidebar.truncate(3);
                self.sidebar.extend(list.into_iter().map(SidebarItem::Playlist));
            }
            Data::Playlists(Err(e)) => self.toast(format!("Playlists: {e:#}")),
            Data::Albums(res) => {
                if let Some(View::Entries(l)) = &mut self.view {
                    if l.title == "Saved Albums" {
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
            }
            Data::TrackChunk { load, tracks, total } => {
                if let Some(l) = self.track_list_mut(load) {
                    l.total = total;
                    let before = l.tracks.len();
                    let n = tracks.len();
                    l.tracks.extend(tracks.into_iter().filter(Track::is_playable_track));
                    l.skipped += (n - (l.tracks.len() - before)) as u32;
                    if before == 0 && !l.tracks.is_empty() {
                        l.state.select(Some(0));
                    }
                }
            }
            Data::TrackLoadDone { load, error } => {
                if let Some(l) = self.track_list_mut(load) {
                    l.loading = false;
                }
                if let Some(e) = error {
                    self.toast(format!("Load failed: {e}"));
                }
            }
            Data::Search { seq, result } => {
                if let Some(View::Entries(l)) = &mut self.view {
                    if matches!(&l.search, Some(s) if s.seq == seq) {
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
    }

    /// Find the track list (current view or navigation stack) for a load id.
    fn track_list_mut(&mut self, load: u64) -> Option<&mut TrackList> {
        self.view
            .iter_mut()
            .chain(self.back.iter_mut())
            .find_map(|v| match v {
                View::Tracks(l) if l.load == load => Some(l),
                _ => None,
            })
    }

    // ------------------------------------------------------------ keys

    fn on_key(&mut self, k: KeyEvent) {
        self.dirty = true;
        if self.help {
            self.help = false;
            return;
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.editing_search() {
            self.on_search_key(k);
            return;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char(' ') => self.player.play_pause(),
            KeyCode::Char('n') => self.player.next(),
            KeyCode::Char('p') => self.player.prev(),
            KeyCode::Char('s') => self.player.shuffle(!self.now.shuffle),
            KeyCode::Char('r') => self.player.repeat(self.now.repeat_ctx, self.now.repeat_track),
            KeyCode::Char('+' | '=') => self.change_volume(true),
            KeyCode::Char('-') => self.change_volume(false),
            KeyCode::Char('>') => self.seek(true),
            KeyCode::Char('<') => self.seek(false),
            KeyCode::Char('/') => self.open_search(),
            KeyCode::Tab => {
                self.focus = if self.focus == Focus::Sidebar { Focus::Content } else { Focus::Sidebar };
            }
            KeyCode::Char('h') | KeyCode::Left => self.focus = Focus::Sidebar,
            KeyCode::Char('l') | KeyCode::Right if self.view.is_some() => self.focus = Focus::Content,
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
                        self.focus = Focus::Content;
                    }
                }
                Focus::Content => self.activate(),
            },
            KeyCode::Esc | KeyCode::Backspace => self.go_back(),
            _ => {}
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

    fn move_selection(&mut self, delta: isize) {
        let (len, sel): (usize, Option<usize>) = match self.focus {
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
        if let Some(View::Entries(EntryList { search: Some(s), .. })) = &mut self.view {
            s.editing = true;
        }
        self.focus = Focus::Content;
    }

    /// Sidebar selections replace the whole view stack (they are roots).
    fn open_sidebar_selection(&mut self) {
        let Some(i) = self.sidebar_state.selected() else { return };
        self.back.clear();
        self.view = None;
        match &self.sidebar[i] {
            SidebarItem::Search => {
                self.view = Some(View::Entries(EntryList {
                    title: "Search".into(),
                    entries: Vec::new(),
                    state: ListState::default(),
                    search: Some(SearchState { query: String::new(), editing: false, seq: 0 }),
                    loading: false,
                }));
            }
            SidebarItem::Liked => {
                let load = self.new_load();
                self.view = Some(View::Tracks(TrackList::new("Liked Songs".into(), load, None)));
                let api = self.api.clone();
                self.spawn_tracks(load, move |on| async move { api.saved_tracks(on).await });
            }
            SidebarItem::Albums => {
                self.view = Some(View::Entries(EntryList {
                    title: "Saved Albums".into(),
                    entries: Vec::new(),
                    state: ListState::default(),
                    search: None,
                    loading: true,
                }));
                let (api, tx) = (self.api.clone(), self.tx.clone());
                tokio::spawn(async move {
                    let _ = tx.send(Event::Data(Data::Albums(api.saved_albums().await)));
                });
            }
            SidebarItem::Playlist(p) => {
                let (id, uri, name) = (p.id.clone(), p.uri.clone(), p.name.clone());
                self.open_playlist(id, uri, name, false);
            }
        }
    }

    fn open_playlist(&mut self, id: String, uri: String, name: String, push: bool) {
        let load = self.new_load();
        let list = View::Tracks(TrackList::new(name, load, Some(uri.clone())));
        if push {
            self.push_view(list);
        } else {
            self.view = Some(list);
        }
        let (api, player) = (self.api.clone(), self.player.clone());
        self.spawn_tracks(load, move |mut on| async move {
            match api.playlist_tracks(&id, &mut on).await {
                // Web API only serves playlists you own; use the streaming session for the rest.
                Err(e) if HttpError::is_forbidden(&e) => player.playlist_tracks(&uri, on).await,
                res => res,
            }
        });
    }

    fn open_album(&mut self, a: &Album) {
        let load = self.new_load();
        self.push_view(View::Tracks(TrackList::new(a.name.clone(), load, Some(a.uri.clone()))));
        let (api, id, name) = (self.api.clone(), a.id.clone(), a.name.clone());
        self.spawn_tracks(load, move |on| async move { api.album_tracks(&id, &name, on).await });
    }

    fn new_load(&mut self) -> u64 {
        self.next_load += 1;
        self.next_load
    }

    fn spawn_playlists(&self) {
        let (api, tx) = (self.api.clone(), self.tx.clone());
        tokio::spawn(async move {
            let _ = tx.send(Event::Data(Data::Playlists(api.my_playlists().await)));
        });
    }

    /// Run a paginated track fetch, streaming chunks back tagged with `load`.
    fn spawn_tracks<F, Fut>(&self, load: u64, f: F)
    where
        F: FnOnce(Box<dyn FnMut(Vec<Track>, u32) + Send>) -> Fut + Send + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send,
    {
        let tx = self.tx.clone();
        tokio::spawn(async move {
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
                        uris.extend(
                            l.entries
                                .iter()
                                .filter_map(|e| match e {
                                    Entry::Track(o) if o.uri != t.uri => Some(o.uri.clone()),
                                    _ => None,
                                }),
                        );
                        self.player.play_tracks(uris, 0);
                    }
                    Entry::Album(a) => self.open_album(&a),
                    Entry::Playlist(p) => self.open_playlist(p.id, p.uri, p.name, true),
                    Entry::Artist(a) => {
                        self.player.play_context(a.uri, 0);
                        self.toast(format!("Playing {}", a.name));
                    }
                }
            }
            None => {}
        }
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
