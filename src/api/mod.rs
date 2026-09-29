pub mod models;

use crate::auth::WebAuth;
use anyhow::{Context, Result, bail};
use futures::{StreamExt, stream};
use models::*;
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use std::time::Duration;

const BASE: &str = "https://api.spotify.com/v1";
const PAGE: u32 = 50;
const CONCURRENCY: usize = 4;

/// Non-success HTTP response, kept typed so callers can react to specific statuses.
#[derive(Debug)]
pub struct HttpError {
    pub path: String,
    pub status: StatusCode,
    pub body: String,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GET {} failed: {} {}", self.path, self.status, self.body)
    }
}

impl std::error::Error for HttpError {}

impl HttpError {
    /// The Web API refuses (403) or hides (404) playlists you don't own, including Spotify's
    /// editorial ones; those must be read through the streaming session instead.
    pub fn is_inaccessible(e: &anyhow::Error) -> bool {
        e.downcast_ref::<HttpError>()
            .is_some_and(|h| matches!(h.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND))
    }
}

#[derive(Clone)]
pub struct Api {
    http: reqwest::Client,
    auth: WebAuth,
}

impl Api {
    pub fn new(auth: WebAuth) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("spotify-tui/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(20))
            .build()?;
        Ok(Self { http, auth })
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T> {
        let url = format!("{BASE}{path}");
        // One retry on 429 honoring Retry-After.
        for attempt in 0..2 {
            let token = self.auth.access_token().await?;
            let resp = self.http.get(&url).bearer_auth(token).query(query).send().await?;
            match resp.status() {
                s if s.is_success() => {
                    return resp.json().await.with_context(|| format!("decoding {path}"));
                }
                StatusCode::TOO_MANY_REQUESTS if attempt == 0 => {
                    let secs = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(1)
                        .min(30);
                    tokio::time::sleep(Duration::from_secs(secs)).await;
                }
                s => {
                    let body = resp.text().await.unwrap_or_default();
                    let msg = body.chars().take(200).collect::<String>();
                    return Err(HttpError { path: path.to_string(), status: s, body: msg }.into());
                }
            }
        }
        bail!("GET {path} rate limited")
    }

    /// Fetch every page of an offset-paginated endpoint. The first page is delivered
    /// immediately via `on_chunk`; the rest are fetched concurrently, in order.
    async fn all_pages<T, U>(
        &self,
        path: &str,
        extra: &[(&str, String)],
        map: impl Fn(T) -> Option<U>,
        mut on_chunk: impl FnMut(Vec<U>, u32),
    ) -> Result<()>
    where
        T: DeserializeOwned,
    {
        let fetch = |offset: u32| {
            let mut q: Vec<(&str, String)> = extra.to_vec();
            q.push(("limit", PAGE.to_string()));
            q.push(("offset", offset.to_string()));
            async move { self.get::<Page<T>>(path, &q).await }
        };
        let first = fetch(0).await?;
        let total = first.total;
        on_chunk(first.items.into_iter().filter_map(&map).collect(), total);

        let offsets: Vec<u32> = (PAGE..total).step_by(PAGE as usize).collect();
        let mut pages = stream::iter(offsets).map(fetch).buffered(CONCURRENCY);
        while let Some(page) = pages.next().await {
            on_chunk(page?.items.into_iter().filter_map(&map).collect(), total);
        }
        Ok(())
    }

    pub async fn my_playlists(&self) -> Result<Vec<Playlist>> {
        let mut out = Vec::new();
        self.all_pages::<Option<Playlist>, _>("/me/playlists", &[], |p| p, |c, _| out.extend(c)).await?;
        Ok(out)
    }

    pub async fn saved_albums(&self) -> Result<Vec<Album>> {
        let mut out = Vec::new();
        self.all_pages::<SavedAlbum, _>("/me/albums", &[], |a| Some(a.album), |c, _| out.extend(c)).await?;
        Ok(out)
    }

    pub async fn saved_tracks(&self, on_chunk: impl FnMut(Vec<Track>, u32)) -> Result<()> {
        self.all_pages::<SavedTrack, _>("/me/tracks", &[], |t| t.track, on_chunk).await
    }

    /// Your most-listened artists (used to seed radio stations).
    pub async fn top_artists(&self) -> Result<Vec<Artist>> {
        let page: Page<Artist> = self
            .get("/me/top/artists", &[("limit", "30".into()), ("time_range", "medium_term".into())])
            .await?;
        Ok(page.items)
    }

    /// Up to 50 most recent plays (cursor-paginated by Spotify, so a single request).
    pub async fn recently_played(&self, mut on_chunk: impl FnMut(Vec<Track>, u32)) -> Result<()> {
        let page: Page<SavedTrack> =
            self.get("/me/player/recently-played", &[("limit", "50".into())]).await?;
        let tracks: Vec<Track> = page.items.into_iter().filter_map(|t| t.track).collect();
        let total = tracks.len() as u32;
        on_chunk(tracks, total);
        Ok(())
    }

    /// What plays after the current track on the active device: queued items first, then the
    /// rest of the context (shuffle order included). Spotify returns about 20 items.
    pub async fn queue(&self) -> Result<Vec<Track>> {
        let q: Queue = self.get("/me/player/queue", &[]).await?;
        Ok(q.queue)
    }

    /// URIs of the playlists behind your last 50 plays, newest first. The Web API can't look up
    /// Spotify-made playlists (404), so names are resolved through the streaming session instead.
    pub async fn recent_playlist_uris(&self) -> Result<Vec<String>> {
        let page: Page<PlayHistoryItem> =
            self.get("/me/player/recently-played", &[("limit", "50".into())]).await?;
        let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
        let mut no_context = 0;
        for item in &page.items {
            match &item.context {
                Some(c) => *kinds.entry(c.kind.clone()).or_default() += 1,
                None => no_context += 1,
            }
        }
        tracing::info!(
            "recent playlists: {} plays, {no_context} without context, contexts by type {kinds:?}",
            page.items.len()
        );
        let mut seen = std::collections::HashSet::new();
        let uris: Vec<String> = page
            .items
            .into_iter()
            .filter_map(|i| i.context)
            .filter(|c| c.kind == "playlist" && seen.insert(c.uri.clone()))
            .map(|c| c.uri)
            .collect();
        tracing::info!("recent playlists: {} distinct playlist contexts: {uris:?}", uris.len());
        Ok(uris)
    }

    pub async fn playlist_tracks(&self, id: &str, on_chunk: impl FnMut(Vec<Track>, u32)) -> Result<()> {
        self.all_pages::<PlaylistItem, _>(&format!("/playlists/{id}/items"), &[], |t| t.item, on_chunk).await
    }

    pub async fn album_tracks(&self, id: &str, album_name: &str, on_chunk: impl FnMut(Vec<Track>, u32)) -> Result<()> {
        let name = album_name.to_string();
        self.all_pages::<Track, _>(
            &format!("/albums/{id}/tracks"),
            &[],
            move |mut t| {
                t.album.get_or_insert_with(|| AlbumRef { name: name.clone() });
                Some(t)
            },
            on_chunk,
        )
        .await
    }

    pub async fn search(&self, q: &str) -> Result<SearchResults> {
        // Feb 2026: limit max is 10 per type.
        self.get(
            "/search",
            &[
                ("q", q.to_string()),
                ("type", "track,album,artist,playlist".into()),
                ("limit", "10".into()),
            ],
        )
        .await
    }
}
