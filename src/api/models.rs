//! Web API response types. Deliberately tolerant: post-2026 responses drop many fields.

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct Page<T> {
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    #[serde(default)]
    pub total: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ArtistRef {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AlbumRef {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Track {
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub artists: Vec<ArtistRef>,
    #[serde(default)]
    pub album: Option<AlbumRef>,
    #[serde(default)]
    pub duration_ms: u32,
}

impl Track {
    pub fn artist_line(&self) -> String {
        self.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
    }

    pub fn album_name(&self) -> &str {
        self.album.as_ref().map_or("", |a| a.name.as_str())
    }

    /// Only real, playable catalog tracks (skips episodes and local files).
    pub fn is_playable_track(&self) -> bool {
        self.uri.starts_with("spotify:track:")
    }
}

#[derive(Debug, Deserialize)]
pub struct SavedTrack {
    pub track: Option<Track>,
}

#[derive(Debug, Deserialize)]
pub struct Queue {
    #[serde(default)]
    pub queue: Vec<Track>,
}

#[derive(Debug, Deserialize)]
pub struct PlayHistoryItem {
    pub context: Option<PlayContext>,
}

#[derive(Debug, Deserialize)]
pub struct PlayContext {
    #[serde(rename = "type")]
    pub kind: String,
    pub uri: String,
}

#[derive(Debug, Deserialize)]
pub struct PlaylistItem {
    /// Renamed `track` -> `item` in the Feb 2026 API.
    #[serde(alias = "track")]
    pub item: Option<Track>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Album {
    pub id: String,
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub artists: Vec<ArtistRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artist {
    pub uri: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct SearchResults {
    pub tracks: Option<Page<Option<Track>>>,
    pub albums: Option<Page<Option<Album>>>,
    pub artists: Option<Page<Option<Artist>>>,
    pub playlists: Option<Page<Option<Playlist>>>,
}

#[derive(Debug, Deserialize)]
pub struct SavedAlbum {
    pub album: Album,
}
