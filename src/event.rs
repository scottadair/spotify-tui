use crate::api::models::{Album, Playlist, SearchResults, Track};
use crate::player::PlaybackEvent;
use crossterm::event::{Event as CtEvent, EventStream, KeyEvent, KeyEventKind};
use futures::StreamExt;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

pub const TICK: Duration = Duration::from_millis(250);

pub enum Event {
    Key(KeyEvent),
    Resize,
    Tick,
    Playback(PlaybackEvent),
    Data(Data),
}

/// Results of background API work.
pub enum Data {
    Playlists(anyhow::Result<Vec<Playlist>>),
    Albums(anyhow::Result<Vec<Album>>),
    /// A chunk of a track list identified by the load id of the view that requested it.
    TrackChunk { load: u64, tracks: Vec<Track>, total: u32 },
    TrackLoadDone { load: u64, error: Option<String> },
    Search { seq: u64, result: anyhow::Result<SearchResults> },
}

/// Terminal input + tick producers.
pub fn spawn_input(tx: UnboundedSender<Event>) {
    let key_tx = tx.clone();
    tokio::spawn(async move {
        let mut stream = EventStream::new();
        while let Some(Ok(ev)) = stream.next().await {
            let ev = match ev {
                CtEvent::Key(k) if k.kind != KeyEventKind::Release => Event::Key(k),
                CtEvent::Resize(..) => Event::Resize,
                _ => continue,
            };
            if key_tx.send(ev).is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        let mut t = tokio::time::interval(TICK);
        loop {
            t.tick().await;
            if tx.send(Event::Tick).is_err() {
                break;
            }
        }
    });
}
