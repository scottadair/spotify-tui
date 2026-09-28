//! Spotify's own browse content. The Web API dropped browse/categories/featured playlists and
//! hides Spotify-owned playlists from search, so this reads through the streaming session:
//! editorial playlists via playlist metadata, a live "popular playlists" list from the mobile
//! browse hub, and radio stations from the radio-apollo service.

use crate::api::models::Playlist;
use anyhow::{Result, bail};
use futures::future::join_all;
use librespot_core::{Session, SpotifyUri};
use librespot_metadata::{Metadata, Playlist as MetaPlaylist};
use reqwest::Method;
use serde::Deserialize;

pub struct Category {
    pub name: &'static str,
    /// Editorial playlist IDs. Names/descriptions are resolved live; IDs that no longer resolve
    /// are dropped, so a stale entry never breaks the page.
    pub ids: &'static [&'static str],
}

pub const CATEGORIES: &[Category] = &[
    Category {
        name: "Charts & New Music",
        ids: &[
            "37i9dQZF1DXcBWIGoYBM5M", // Today's Top Hits
            "37i9dQZEVXbMDoHDwVN2tF", // Top 50 - Global
            "37i9dQZEVXbLRQDuF5jeBp", // Top 50 - USA
            "37i9dQZF1DX4JAvHpjipBk", // New Music Friday
            "37i9dQZF1DX0kbJZpiYdZl", // Hot Hits USA
            "37i9dQZF1DXbYM3nMM0oPk", // Mega Hit Mix
        ],
    },
    Category {
        name: "Pop",
        ids: &[
            "37i9dQZF1DWUa8ZRTfalHk", // Pop Rising
            "37i9dQZF1DX4UtSsGT1Sbe", // All Out 80s
            "37i9dQZF1DXbTxeAdrVG2l", // All Out 90s
            "37i9dQZF1DX4o1oenSJRJd", // All Out 2000s
            "37i9dQZF1DX5Ejj0EkURtP", // All Out 2010s
        ],
    },
    Category {
        name: "Hip-Hop",
        ids: &[
            "37i9dQZF1DX0XUsuxWHRQd", // RapCaviar
            "37i9dQZF1DX2RxBh64BHjQ", // Most Necessary
            "37i9dQZF1DWY4xHQp97fN6", // Get Turnt
            "37i9dQZF1DX6GwdWRQMQpq", // Feelin' Myself
            "37i9dQZF1DX186v583rmzp", // I Love My '90s Hip-Hop
        ],
    },
    Category {
        name: "R&B",
        ids: &[
            "37i9dQZF1DX4SBhb3fqCJd", // RNB X
            "37i9dQZF1DX6VDO8a6cQME", // I Love My '90s R&B
            "37i9dQZF1DWXbttAJcbphz", // I Love My '10s R&B
        ],
    },
    Category {
        name: "Rock & Metal",
        ids: &[
            "37i9dQZF1DWXRqgorJj26U", // Rock Classics
            "37i9dQZF1DX1spT6G94GFC", // 80s Rock Anthems
            "37i9dQZF1DX1rVvRgjX59F", // 90s Rock Anthems
            "37i9dQZF1DX3oM43CtKnRV", // 00s Rock Anthems
            "37i9dQZF1DX9GRpeH4CL0S", // Essential Alternative
            "37i9dQZF1DXcF6B6QPhFDv", // MARROW
        ],
    },
    Category {
        name: "Indie",
        ids: &[
            "37i9dQZF1DWWEcRhUVtL8n", // Indie Pop
            "37i9dQZF1DX2Nc3B70tvx0", // Indie's Top 50
            "37i9dQZF1DX2sUQwD7tbmL", // Feel-Good Indie Rock
            "37i9dQZF1DXdbXrPNafg9d", // All New Indie
        ],
    },
    Category {
        name: "Chill & Focus",
        ids: &[
            "37i9dQZF1DX4WYpdgoIcn6", // Chill Hits
            "37i9dQZF1DWWQRwui0ExPn", // lofi beats
            "37i9dQZF1DX2yvmlOdMYzV", // Lowkey
            "37i9dQZF1DX4sWSpwq3LiO", // Peaceful Piano
            "37i9dQZF1DWZeKCadgRdKQ", // Deep Focus
            "37i9dQZF1DX8NTLI2TtZa6", // Intense Studying
            "37i9dQZF1DX9sIqqvKsjG8", // Instrumental Study
        ],
    },
    Category {
        name: "Sleep & Relax",
        ids: &[
            "37i9dQZF1DWZd79rJ6a7lp", // Sleep
            "37i9dQZF1DX3Ogo9pFvBkY", // Ambient Relaxation
            "37i9dQZF1DWXe9gFZP0gtP", // Stress Relief
        ],
    },
    Category {
        name: "Workout",
        ids: &[
            "37i9dQZF1DX76Wlfdnj7AP", // Beast Mode
            "37i9dQZF1DXdxcBWuJkbcy", // Gym Hits
            "37i9dQZF1DX32NsLKyzScr", // Power Hour
            "37i9dQZF1DWSJHnPb1f0X3", // Cardio
            "37i9dQZF1DX70RN3TfWWJh", // Workout
        ],
    },
    Category {
        name: "Party & Mood",
        ids: &[
            "37i9dQZF1DXa2PvUpywmrr", // Party Hits
            "37i9dQZF1DXaXB8fQg7xif", // Dance Party
            "37i9dQZF1DX3rxVfibe1L0", // Mood Booster
            "37i9dQZF1DXdPec7aLTmlC", // Happy Hits!
            "37i9dQZF1DX0BcQWzuB7ZO", // Dance Hits
        ],
    },
    Category {
        name: "Dance & Electronic",
        ids: &[
            "37i9dQZF1DX4dyzvuaRJ0n", // mint
            "37i9dQZF1DX8tZsk68tuDw", // Dance Rising
            "37i9dQZF1DXa8NOEUWPn9W", // Housewerk
            "37i9dQZF1DX0AMssoUKCz7", // Tropical House
        ],
    },
    Category {
        name: "Country",
        ids: &[
            "37i9dQZF1DX1lVhptIYRda", // Hot Country
            "37i9dQZF1DX13ZzXoot6Jc", // Country Favourites
        ],
    },
    Category {
        name: "Latin",
        ids: &[
            "37i9dQZF1DX10zKzsJ2jva", // Viva Latino
            "37i9dQZF1DXbLMw3ry7d7k", // Latin Hit Mix
        ],
    },
    Category {
        name: "Jazz & Classical",
        ids: &[
            "37i9dQZF1DWVqfgj8NZEp1", // Coffee Table Jazz
            "37i9dQZF1DXbITWG1ZJKYt", // Jazz Classics
            "37i9dQZF1DX0SM0LYsmbMT", // Jazz Vibes
            "37i9dQZF1DX7YCknf2jT6s", // State of Jazz
            "37i9dQZF1DWTR4ZOXTfd9K", // Blue Note
            "37i9dQZF1DWWEJlAGA9gs0", // Classical Essentials
        ],
    },
];

fn playlist(id: &str, name: String, description: String) -> Playlist {
    Playlist { id: id.to_string(), uri: format!("spotify:playlist:{id}"), name, description }
}

/// Resolve a category's playlists concurrently, keeping catalog order.
pub async fn category_playlists(session: &Session, cat: &Category) -> Vec<Playlist> {
    let lookups = cat.ids.iter().map(|id| async move {
        let uri = SpotifyUri::from_uri(&format!("spotify:playlist:{id}")).ok()?;
        let p = MetaPlaylist::get(session, &uri).await.ok()?;
        Some(playlist(id, p.attributes.name, p.attributes.description))
    });
    join_all(lookups).await.into_iter().flatten().collect()
}

#[derive(Deserialize)]
struct Hub {
    #[serde(default)]
    body: Vec<HubItem>,
}

#[derive(Deserialize)]
struct HubItem {
    #[serde(default)]
    text: HubText,
    metadata: Option<HubMeta>,
}

#[derive(Deserialize, Default)]
struct HubText {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
struct HubMeta {
    #[serde(default)]
    uri: String,
}

/// Spotify's current popular editorial playlists (undocumented mobile browse hub). Fails soft:
/// callers show the error and the curated categories keep working.
pub async fn popular_playlists(session: &Session) -> Result<Vec<Playlist>> {
    let body = session
        .spclient()
        .request_as_json(&Method::GET, "/hubview-mobile-v1/browse?platform=ios&locale=en", None, None)
        .await?;
    let hub: Hub = serde_json::from_slice(&body)?;
    let out: Vec<Playlist> = hub
        .body
        .into_iter()
        .filter_map(|i| {
            let uri = i.metadata?.uri;
            let id = uri.strip_prefix("spotify:playlist:")?.to_string();
            Some(playlist(&id, i.text.title, i.text.description))
        })
        .collect();
    if out.is_empty() {
        bail!("browse hub returned no playlists");
    }
    Ok(out)
}

#[derive(Deserialize)]
struct Station {
    #[serde(default)]
    tracks: Vec<StationTrack>,
}

#[derive(Deserialize)]
struct StationTrack {
    uri: String,
}

/// Track URIs of a radio station seeded by an artist or track URI.
pub async fn station_tracks(session: &Session, seed_uri: &str) -> Result<Vec<String>> {
    let body = session
        .spclient()
        .get_apollo_station("stations", seed_uri, Some(50), Vec::new(), true)
        .await?;
    let station: Station = serde_json::from_slice(&body)?;
    let uris: Vec<String> = station
        .tracks
        .into_iter()
        .map(|t| t.uri)
        .filter(|u| u.starts_with("spotify:track:"))
        .collect();
    if uris.is_empty() {
        bail!("station has no tracks");
    }
    Ok(uris)
}
