use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// librespot's public client id. Streaming logins must use it.
pub const LIBRESPOT_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
pub const LIBRESPOT_REDIRECT: &str = "http://127.0.0.1:8898/login";
pub const CUSTOM_REDIRECT: &str = "http://127.0.0.1:8888/callback";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Optional: client ID of your own app (https://developer.spotify.com/dashboard, redirect
    /// URI http://127.0.0.1:8888/callback) for Web API calls with your own rate limits.
    /// Empty = use the same login as the streaming device (single sign-in).
    pub client_id: String,
    /// Name shown in Spotify Connect device lists.
    pub device_name: String,
    /// Streaming bitrate: 96, 160 or 320.
    pub bitrate: u16,
    /// Initial volume, 0-100 (only used until a volume is cached).
    pub initial_volume: u8,
    pub gapless: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            device_name: "spotify-tui".into(),
            bitrate: 320,
            initial_volume: 50,
            gapless: true,
        }
    }
}

pub struct Paths {
    pub config_file: PathBuf,
    pub cache_dir: PathBuf,
    pub log_file: PathBuf,
}

impl Paths {
    pub fn resolve() -> Result<Self> {
        let dirs = ProjectDirs::from("", "", "spotify-tui").context("cannot determine home directory")?;
        Ok(Self {
            config_file: dirs.config_dir().join("config.toml"),
            cache_dir: dirs.cache_dir().to_path_buf(),
            log_file: dirs.cache_dir().join("spotify-tui.log"),
        })
    }

    pub fn web_token(&self) -> PathBuf {
        self.cache_dir.join("web_token.json")
    }

    pub fn librespot_cache(&self) -> PathBuf {
        self.cache_dir.join("librespot")
    }
}

impl Config {
    /// Load the config file; a missing file means all defaults.
    pub fn load(paths: &Paths) -> Result<Self> {
        if !paths.config_file.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&paths.config_file)?;
        toml::from_str(&text).with_context(|| format!("invalid config {}", paths.config_file.display()))
    }

    /// (client id, redirect URI) used for Web API auth.
    pub fn oauth_client(&self) -> (&str, &str) {
        let id = self.client_id.trim();
        if id.is_empty() {
            (LIBRESPOT_CLIENT_ID, LIBRESPOT_REDIRECT)
        } else {
            (id, CUSTOM_REDIRECT)
        }
    }

    pub fn uses_default_client(&self) -> bool {
        self.client_id.trim().is_empty()
    }
}
