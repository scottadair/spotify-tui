use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// librespot's public client id. The streaming session must be authorised by it. It is
/// heavily rate limited on the Web API, so it is only used for streaming.
pub const LIBRESPOT_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
pub const LIBRESPOT_REDIRECT: &str = "http://127.0.0.1:8898/login";
pub const REDIRECT_URI: &str = "http://127.0.0.1:8888/callback";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Client ID of your own Spotify app (required; used for all Web API calls).
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
    /// Load the config file. `client_id` is required for Web API access.
    pub fn load(paths: &Paths) -> Result<Self> {
        if !paths.config_file.exists() {
            if let Some(parent) = paths.config_file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&paths.config_file, toml::to_string_pretty(&Config::default())?)?;
        }
        let text = std::fs::read_to_string(&paths.config_file)?;
        let cfg: Config = toml::from_str(&text)
            .with_context(|| format!("invalid config {}", paths.config_file.display()))?;
        if cfg.client_id.trim().is_empty() {
            bail!(
                "Set `client_id` in {}\n\
                 1. Create an app at https://developer.spotify.com/dashboard\n\
                 2. Add redirect URI: {REDIRECT_URI}\n\
                 3. Copy its Client ID into the config file and re-run.",
                paths.config_file.display()
            );
        }
        Ok(cfg)
    }
}
