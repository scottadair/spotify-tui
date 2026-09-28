//! Embedded Spotify Connect device (librespot). Playback state is driven purely by
//! player events, so the UI never polls the Web API.

use crate::config::{Config, LIBRESPOT_CLIENT_ID, LIBRESPOT_REDIRECT, Paths};
use crate::api::models::{AlbumRef, ArtistRef, Track as ApiTrack};
use anyhow::{Context, Result};
use futures::{StreamExt, future::join_all, stream};
use librespot_core::SpotifyUri;
use librespot_metadata::{Metadata, Playlist as MetaPlaylist, Track as MetaTrack};
use librespot_connect::{ConnectConfig, LoadRequest, LoadRequestOptions, PlayingTrack, Spirc};
use librespot_core::{Session, SessionConfig, authentication::Credentials, cache::Cache};
use librespot_metadata::audio::UniqueFields;
use librespot_oauth::OAuthClientBuilder;
use librespot_playback::{
    audio_backend,
    config::{AudioFormat, Bitrate, PlayerConfig},
    mixer::{Mixer, MixerConfig, softmixer::SoftMixer},
    player::{Player, PlayerEvent},
};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;



#[derive(Debug, Clone, Default)]
pub struct TrackInfo {
    pub uri: String,
    pub name: String,
    pub artists: String,
    pub album: String,
    pub duration_ms: u32,
}

/// Player state changes, decoupled from librespot's event type.
#[derive(Debug, Clone)]
pub enum PlaybackEvent {
    Loading,
    Track(TrackInfo),
    Playing { position_ms: u32 },
    Paused { position_ms: u32 },
    Stopped,
    Seeked { position_ms: u32 },
    Volume(u16),
    Shuffle(bool),
    Repeat { context: bool, track: bool },
    Unavailable,
}

#[derive(Clone)]
pub struct PlayerHandle {
    spirc: Arc<Spirc>,
    mixer: Arc<SoftMixer>,
    session: Session,
}

const STREAM_SCOPES: &[&str] = &[
    "streaming",
    "app-remote-control",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
];

/// Credentials from cache, else a one-time interactive login with librespot's client id
/// (the streaming session only accepts tokens from it). Must run before the TUI starts.
async fn credentials(cache: &Cache) -> Result<Credentials> {
    if let Some(c) = cache.credentials() {
        return Ok(c);
    }
    println!("Authorising the streaming device (one-time)...");
    let token = OAuthClientBuilder::new(LIBRESPOT_CLIENT_ID, LIBRESPOT_REDIRECT, STREAM_SCOPES.to_vec())
        .open_in_browser()
        .with_custom_message("Streaming device authorised. You can close this tab.")
        .build()
        .context("building streaming OAuth client")?
        .get_access_token_async()
        .await
        .context("streaming login failed")?;
    Ok(Credentials::with_access_token(token.access_token))
}

pub struct Started {
    pub handle: PlayerHandle,
    /// Initial volume, 0..=u16::MAX.
    pub volume: u16,
}

pub async fn start(cfg: &Config, paths: &Paths, events: UnboundedSender<PlaybackEvent>) -> Result<Started> {
    let cache = Cache::new(
        Some(paths.librespot_cache()),
        Some(paths.librespot_cache()),
        None,
        None,
    )?;
    let creds = credentials(&cache).await?;

    let session = Session::new(SessionConfig::default(), Some(cache.clone()));

    let mixer = Arc::new(SoftMixer::open(MixerConfig::default())?);
    let volume = cache
        .volume()
        .unwrap_or_else(|| (u32::from(cfg.initial_volume.min(100)) * u32::from(u16::MAX) / 100) as u16);
    mixer.set_volume(volume);

    let bitrate = match cfg.bitrate {
        0..=96 => Bitrate::Bitrate96,
        97..=160 => Bitrate::Bitrate160,
        _ => Bitrate::Bitrate320,
    };
    let player_cfg = PlayerConfig {
        bitrate,
        gapless: cfg.gapless,
        ..Default::default()
    };
    let backend = audio_backend::find(None).context("no audio backend compiled in")?;
    let player = Player::new(player_cfg, session.clone(), mixer.get_soft_volume(), move || {
        backend(None, AudioFormat::default())
    });

    // Forward player events before Spirc starts so nothing is missed.
    let mut rx = player.get_player_event_channel();
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let Some(ev) = map_event(ev) {
                if events.send(ev).is_err() {
                    break;
                }
            }
        }
    });

    let connect_cfg = ConnectConfig {
        name: cfg.device_name.clone(),
        initial_volume: volume,
        ..Default::default()
    };
    let handle_session = session.clone();
    let (spirc, task) = Spirc::new(connect_cfg, session, creds, player, mixer.clone())
        .await
        .context("connecting to Spotify (delete the librespot cache dir to re-authorise)")?;
    tokio::spawn(task);

    Ok(Started { handle: PlayerHandle { spirc: Arc::new(spirc), mixer, session: handle_session }, volume })
}

fn map_event(ev: PlayerEvent) -> Option<PlaybackEvent> {
    Some(match ev {
        PlayerEvent::Loading { .. } => PlaybackEvent::Loading,
        PlayerEvent::Playing { position_ms, .. } => PlaybackEvent::Playing { position_ms },
        PlayerEvent::Paused { position_ms, .. } => PlaybackEvent::Paused { position_ms },
        PlayerEvent::Stopped { .. } => PlaybackEvent::Stopped,
        PlayerEvent::Seeked { position_ms, .. }
        | PlayerEvent::PositionCorrection { position_ms, .. } => PlaybackEvent::Seeked { position_ms },
        PlayerEvent::VolumeChanged { volume } => PlaybackEvent::Volume(volume),
        PlayerEvent::ShuffleChanged { shuffle } => PlaybackEvent::Shuffle(shuffle),
        PlayerEvent::RepeatChanged { context, track } => PlaybackEvent::Repeat { context, track },
        PlayerEvent::Unavailable { .. } => PlaybackEvent::Unavailable,
        PlayerEvent::TrackChanged { audio_item } => {
            let (artists, album) = match &audio_item.unique_fields {
                UniqueFields::Track { artists, album, .. } => (
                    artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "),
                    album.clone(),
                ),
                UniqueFields::Local { artists, album, .. } => {
                    (artists.clone().unwrap_or_default(), album.clone().unwrap_or_default())
                }
                UniqueFields::Episode { show_name, .. } => (show_name.clone(), String::new()),
            };
            PlaybackEvent::Track(TrackInfo {
                uri: audio_item.uri.clone(),
                name: audio_item.name.clone(),
                artists,
                album,
                duration_ms: audio_item.duration_ms,
            })
        }
        _ => return None,
    })
}

impl PlayerHandle {
    /// Load a playlist's tracks through the streaming session's metadata endpoints. Unlike the
    /// Web API (owned playlists only since Feb 2026) this works for any playlist you can open.
    pub async fn playlist_tracks(
        &self,
        uri: &str,
        mut on_chunk: impl FnMut(Vec<ApiTrack>, u32),
    ) -> Result<()> {
        let uri = SpotifyUri::from_uri(uri)?;
        let list = MetaPlaylist::get(&self.session, &uri).await?;
        let ids: Vec<SpotifyUri> = list
            .tracks()
            .filter(|u| matches!(u, SpotifyUri::Track { .. }))
            .cloned()
            .collect();
        let total = ids.len() as u32;
        let session = &self.session;
        let owned: Vec<Vec<SpotifyUri>> = ids.chunks(20).map(<[_]>::to_vec).collect();
        let mut chunks = stream::iter(owned).map(|c| fetch_chunk(session, c)).buffered(4);
        while let Some(results) = chunks.next().await {
            let tracks = results.into_iter().flatten().filter_map(to_api_track).collect();
            on_chunk(tracks, total);
        }
        Ok(())
    }

    pub fn play_pause(&self) {
        let _ = self.spirc.play_pause();
    }
    pub fn next(&self) {
        let _ = self.spirc.next();
    }
    pub fn prev(&self) {
        let _ = self.spirc.prev();
    }
    pub fn seek(&self, ms: u32) {
        let _ = self.spirc.set_position_ms(ms);
    }
    pub fn shuffle(&self, on: bool) {
        let _ = self.spirc.shuffle(on);
    }
    /// Cycle repeat: off -> context -> track -> off, given the current mode.
    pub fn repeat(&self, context: bool, track: bool) {
        let _ = match (context, track) {
            (false, false) => self.spirc.repeat(true),
            (true, false) => self.spirc.repeat_track(true),
            _ => self.spirc.repeat(false).and_then(|_| self.spirc.repeat_track(false)),
        };
    }
    pub fn set_volume(&self, volume: u16) {
        let _ = self.spirc.set_volume(volume);
        self.mixer.set_volume(volume);
    }

    fn opts(index: usize) -> LoadRequestOptions {
        LoadRequestOptions {
            start_playing: true,
            playing_track: Some(PlayingTrack::Index(index as u32)),
            ..Default::default()
        }
    }

    /// Spirc ignores every command unless this device is the active Connect device, and another
    /// device may have taken over since we last played. Activating first is a no-op if we're active.
    fn load(&self, request: LoadRequest) {
        let _ = self.spirc.activate();
        let _ = self.spirc.load(request);
    }

    /// Play a playlist/album/artist context starting at `index`.
    pub fn play_context(&self, uri: String, index: usize) {
        self.load(LoadRequest::from_context_uri(uri, Self::opts(index)));
    }

    /// Play an explicit list of track URIs starting at `index`.
    pub fn play_tracks(&self, uris: Vec<String>, index: usize) {
        self.load(LoadRequest::from_tracks(uris, Self::opts(index)));
    }

    pub fn shutdown(&self) {
        let _ = self.spirc.shutdown();
    }
}

fn to_api_track(t: MetaTrack) -> Option<ApiTrack> {
    Some(ApiTrack {
        uri: t.id.to_uri().ok()?,
        name: t.name,
        artists: t.artists.iter().map(|a| ArtistRef { name: a.name.clone() }).collect(),
        album: Some(AlbumRef { name: t.album.name }),
        duration_ms: t.duration.max(0) as u32,
    })
}

async fn fetch_chunk(session: &Session, ids: Vec<SpotifyUri>) -> Vec<Result<MetaTrack, librespot_core::Error>> {
    join_all(ids.iter().map(|id| MetaTrack::get(session, id))).await
}
