//! Spotify's own browse content, read the way the official client does: the persisted GraphQL
//! queries behind "Browse all" on api-partner.spotify.com (undocumented, authorised with the
//! streaming session's login5 token plus its client token), and radio stations from the
//! radio-apollo service. The Web API dropped browse/categories and hides Spotify-owned
//! playlists from search, so this is the only way to reach the full editorial catalogue.

use crate::api::models::{Album, ArtistRef, Playlist};
use anyhow::{Context, Result, bail};
use librespot_core::Session;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{sync::LazyLock, time::Duration};

const ENDPOINT: &str = "https://api-partner.spotify.com/pathfinder/v2/query";
const INTEGRATION: &str = "INTEGRATION_WEB_PLAYER";
const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
const BROWSE_ALL: &str = "dbd8b55e09a58afc52eab438bc228ba28fd72ac2f2148c6c26354980e4579001";
const BROWSE_PAGE: &str = "f5c4e6d668f5716464a231c1cc8b22c1cbf6ad68b09929fd7de813a30581298b";
const BROWSE_SECTION: &str = "b13c1cccbfcb6947753c2613411b3566485c21fd5f36d80a80bb64be61ba2d51";

/// Shelves per `browsePage` request and items per `browseSection` request.
const PAGE_STEP: u32 = 10;
const SECTION_STEP: u32 = 100;

static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder().timeout(Duration::from_secs(30)).build().unwrap_or_default()
});

/// A browse page (`spotify:page:…`): a top-level category such as "Rock", or a sub-page such
/// as "90s".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Category {
    pub uri: String,
    pub name: String,
}

/// One shelf on a page ("Rock Classics").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub uri: String,
    pub title: String,
    pub total: u32,
    /// The shelf's contents when the page response carried all of them (always the case for
    /// "Related content" shelves, which `browseSection` can't serve); empty otherwise.
    #[serde(default)]
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Item {
    Playlist(Playlist),
    Album(Album),
    Page(Category),
}

// ---------------------------------------------------------------- wire types

#[derive(Deserialize)]
struct Envelope {
    data: Option<Value>,
    #[serde(default)]
    errors: Vec<Value>,
}

#[derive(Deserialize, Default)]
struct Sections {
    #[serde(default)]
    items: Vec<WireSection>,
    #[serde(default, rename = "pagingInfo")]
    paging: Paging,
}

#[derive(Deserialize, Default)]
struct Paging {
    #[serde(rename = "nextOffset")]
    next_offset: Option<u32>,
}

#[derive(Deserialize)]
struct WireSection {
    uri: String,
    #[serde(default)]
    data: SectionData,
    #[serde(rename = "sectionItems")]
    items: SectionItems,
}

#[derive(Deserialize, Default)]
struct SectionData {
    title: Option<Label>,
}

#[derive(Deserialize)]
struct Label {
    #[serde(default, rename = "transformedLabel")]
    text: String,
}

#[derive(Deserialize, Default)]
struct SectionItems {
    #[serde(default)]
    items: Vec<WireItem>,
    #[serde(default, rename = "totalCount")]
    total: u32,
    #[serde(default, rename = "pagingInfo")]
    paging: Paging,
}

#[derive(Deserialize)]
struct WireItem {
    uri: String,
    content: Content,
}

#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum Content {
    #[serde(rename = "PlaylistResponseWrapper")]
    Playlist { data: WirePlaylist },
    #[serde(rename = "AlbumResponseWrapper")]
    Album { data: WireAlbum },
    #[serde(rename = "BrowseSectionContainerWrapper")]
    Page { data: WirePage },
    /// Podcasts, audiobooks, links… nothing this client can play.
    #[serde(other)]
    Unsupported,
}

#[derive(Deserialize)]
struct WirePlaylist {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
struct WireAlbum {
    #[serde(default)]
    name: String,
    #[serde(default)]
    artists: WireArtists,
}

#[derive(Deserialize, Default)]
struct WireArtists {
    #[serde(default)]
    items: Vec<WireArtist>,
}

#[derive(Deserialize)]
struct WireArtist {
    profile: Option<WireProfile>,
}

#[derive(Deserialize)]
struct WireProfile {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct WirePage {
    data: Option<WireCard>,
}

#[derive(Deserialize)]
struct WireCard {
    #[serde(rename = "cardRepresentation")]
    card: Option<WireCardRep>,
}

#[derive(Deserialize)]
struct WireCardRep {
    title: Option<Label>,
}

impl WireItem {
    fn into_item(self) -> Option<Item> {
        let uri = self.uri;
        match self.content {
            Content::Playlist { data } => {
                let id = uri.strip_prefix("spotify:playlist:")?.to_string();
                Some(Item::Playlist(Playlist { id, uri, name: data.name, description: data.description }))
            }
            Content::Album { data } => {
                let id = uri.strip_prefix("spotify:album:")?.to_string();
                let artists = data
                    .artists
                    .items
                    .into_iter()
                    .filter_map(|a| a.profile)
                    .map(|p| ArtistRef { name: p.name })
                    .collect();
                Some(Item::Album(Album { id, uri, name: data.name, artists }))
            }
            Content::Page { data } => {
                if !uri.starts_with("spotify:page:") {
                    return None;
                }
                let name = data.data?.card?.title?.text;
                Some(Item::Page(Category { uri, name }))
            }
            Content::Unsupported => None,
        }
    }
}

// ---------------------------------------------------------------- transport

/// One persisted query. Returns the `data` object; GraphQL errors without data are failures.
async fn query(session: &Session, operation: &str, hash: &str, mut variables: Value) -> Result<Value> {
    variables["browseEndUserIntegration"] = json!(INTEGRATION);
    variables["includeEpisodeContentRatingsV2"] = json!(true);
    let body = json!({
        "variables": variables,
        "operationName": operation,
        "extensions": { "persistedQuery": { "version": 1, "sha256Hash": hash } },
    });

    let token = session.login5().auth_token().await.context("login5 token")?;
    let client_token = session.spclient().client_token().await.context("client token")?;
    let resp = HTTP
        .post(ENDPOINT)
        .header("authorization", format!("Bearer {}", token.access_token))
        .header("client-token", client_token)
        .header("app-platform", "WebPlayer")
        .header("accept", "application/json")
        .header("user-agent", USER_AGENT)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("{operation} request"))?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    if !status.is_success() {
        bail!("{operation}: HTTP {status}");
    }
    let env: Envelope = serde_json::from_slice(&bytes).with_context(|| format!("{operation} response"))?;
    match env.data {
        Some(d) => Ok(d),
        None => bail!("{operation}: {}", env.errors.first().map_or("no data".into(), Value::to_string)),
    }
}

/// Deserialize `data.<key>.sections`.
fn sections_at(mut data: Value, key: &str) -> Result<Sections> {
    let node = data.get_mut(key).and_then(|c| c.get_mut("sections")).map(Value::take);
    let node = node.with_context(|| format!("response has no {key}.sections"))?;
    Ok(serde_json::from_value(node)?)
}

// ---------------------------------------------------------------- browse

/// The live "Browse all" catalogue: every top-level category, in Spotify's order.
pub async fn categories(session: &Session) -> Result<Vec<Category>> {
    let vars = json!({
        "pagePagination": { "offset": 0, "limit": PAGE_STEP },
        "sectionPagination": { "offset": 0, "limit": 99 },
    });
    let data = query(session, "browseAll", BROWSE_ALL, vars).await?;
    let out: Vec<Category> = sections_at(data, "browseStart")?
        .items
        .into_iter()
        .flat_map(|s| s.items.items)
        .filter_map(WireItem::into_item)
        .filter_map(|i| match i {
            Item::Page(c) => Some(c),
            _ => None,
        })
        .collect();
    if out.is_empty() {
        bail!("Browse all returned no categories");
    }
    Ok(out)
}

/// Every shelf of a page. Empty shelves are dropped.
pub async fn page_groups(session: &Session, page_uri: &str) -> Result<Vec<Group>> {
    let mut groups = Vec::new();
    let mut offset = 0;
    loop {
        let vars = json!({
            "uri": page_uri,
            "pagePagination": { "offset": offset, "limit": PAGE_STEP },
            // Spotify caps this at 50; longer shelves are completed by `group_items`.
            "sectionPagination": { "offset": 0, "limit": 50 },
        });
        let data = query(session, "browsePage", BROWSE_PAGE, vars).await?;
        let sections = sections_at(data, "browse")?;
        for s in sections.items {
            let total = s.items.total;
            let complete = s.items.items.len() as u32 >= total;
            let items: Vec<Item> = s.items.items.into_iter().filter_map(WireItem::into_item).collect();
            // Nothing this client can play (podcasts, audiobooks…).
            if total == 0 || (complete && items.is_empty()) {
                continue;
            }
            let items = if complete { items } else { Vec::new() };
            groups.push(Group { title: s.data.title.map_or_else(String::new, |t| t.text), uri: s.uri, total, items });
        }
        match sections.paging.next_offset {
            Some(next) if next > offset => offset = next,
            _ => break,
        }
    }
    if groups.is_empty() {
        bail!("page has no playlists or albums");
    }
    Ok(groups)
}

/// Everything on one shelf: what the page already carried, else paged in from `browseSection`.
pub async fn group_items(session: &Session, group: Group) -> Result<Vec<Item>> {
    if !group.items.is_empty() {
        return Ok(group.items);
    }
    let section_uri = &group.uri;
    let mut items = Vec::new();
    let mut offset = 0;
    loop {
        let vars = json!({
            "uri": section_uri,
            "pagination": { "offset": offset, "limit": SECTION_STEP },
        });
        let mut data = query(session, "browseSection", BROWSE_SECTION, vars).await?;
        let node = data.get_mut("browseSection").and_then(|s| s.get_mut("sectionItems")).map(Value::take);
        let node: SectionItems = serde_json::from_value(node.context("response has no browseSection")?)?;
        items.extend(node.items.into_iter().filter_map(WireItem::into_item));
        match node.paging.next_offset {
            Some(next) if next > offset => offset = next,
            _ => break,
        }
    }
    if items.is_empty() {
        bail!("shelf has no playlists or albums");
    }
    Ok(items)
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
