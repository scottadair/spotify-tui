//! Spotify Web API auth: PKCE (via librespot-oauth) with a cached, auto-refreshed token.

use crate::config::{Config, Paths, REDIRECT_URI};
use anyhow::{Context, Result};
use librespot_oauth::{OAuthClient, OAuthClientBuilder, OAuthToken};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

const SCOPES: &[&str] = &[
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-private",
    "playlist-modify-public",
    "user-library-read",
    "user-library-modify",
    "user-follow-read",
    "user-top-read",
    "user-read-recently-played",
    "user-read-private",
];

/// Refresh this long before actual expiry.
const SKEW: Duration = Duration::from_secs(60);

#[derive(Serialize, Deserialize)]
struct Stored {
    client_id: String,
    refresh_token: String,
}

struct Current {
    access_token: String,
    expires_at: Instant,
    refresh_token: String,
}

#[derive(Clone)]
pub struct WebAuth {
    oauth: Arc<OAuthClient>,
    file: PathBuf,
    client_id: String,
    current: Arc<Mutex<Current>>,
}

impl WebAuth {
    /// Use the cached refresh token if it works, otherwise run the interactive browser flow.
    /// Must run before the TUI starts (prints the URL to stdout).
    pub async fn login(cfg: &Config, paths: &Paths) -> Result<Self> {
        let client_id = cfg.client_id.trim();
        let oauth = OAuthClientBuilder::new(client_id, REDIRECT_URI, SCOPES.to_vec())
            .open_in_browser()
            .with_custom_message("Logged in to spotify-tui. You can close this tab.")
            .build()
            .context("building OAuth client")?;
        let file = paths.web_token();

        let cached = std::fs::read_to_string(&file)
            .ok()
            .and_then(|s| serde_json::from_str::<Stored>(&s).ok())
            .filter(|s| s.client_id == client_id);

        let token = match cached {
            Some(s) => match oauth.refresh_token_async(&s.refresh_token).await {
                Ok(mut t) => {
                    if t.refresh_token.is_empty() {
                        t.refresh_token = s.refresh_token;
                    }
                    Some(t)
                }
                Err(e) => {
                    tracing::warn!("refresh failed, re-authenticating: {e}");
                    None
                }
            },
            None => None,
        };
        let token = match token {
            Some(t) => t,
            None => oauth.get_access_token_async().await.context("Spotify login failed")?,
        };

        let this = Self {
            oauth: Arc::new(oauth),
            file,
            client_id: client_id.to_string(),
            current: Arc::new(Mutex::new(Current {
                access_token: String::new(),
                expires_at: Instant::now(),
                refresh_token: String::new(),
            })),
        };
        this.store(&mut *this.current.lock().await, token);
        Ok(this)
    }

    fn store(&self, cur: &mut Current, t: OAuthToken) {
        cur.access_token = t.access_token;
        cur.expires_at = t.expires_at;
        if !t.refresh_token.is_empty() {
            cur.refresh_token = t.refresh_token;
        }
        if let Some(dir) = self.file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let json = serde_json::to_string(&Stored {
            client_id: self.client_id.clone(),
            refresh_token: cur.refresh_token.clone(),
        });
        if let Ok(json) = json {
            write_private(&self.file, json.as_bytes());
        }
    }

    /// A valid access token, refreshing if near expiry. Concurrent callers share one refresh.
    pub async fn access_token(&self) -> Result<String> {
        let mut cur = self.current.lock().await;
        if Instant::now() + SKEW >= cur.expires_at {
            let t = self
                .oauth
                .refresh_token_async(&cur.refresh_token)
                .await
                .context("token refresh failed")?;
            self.store(&mut cur, t);
        }
        Ok(cur.access_token.clone())
    }
}

fn write_private(path: &std::path::Path, data: &[u8]) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path);
    if let Ok(mut f) = f {
        let _ = f.write_all(data);
    }
}
