use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

/// librespot's public client id. The streaming session must be authorised by it. It is
/// heavily rate limited on the Web API, so it is only used for streaming.
pub const LIBRESPOT_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
pub const LIBRESPOT_REDIRECT: &str = "http://127.0.0.1:8898/login";
pub const REDIRECT_URI: &str = "http://127.0.0.1:8888/callback";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Even out loudness between tracks using Spotify's per-track gain metadata.
    pub normalisation: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            device_name: "spotify-tui".into(),
            bitrate: 320,
            initial_volume: 50,
            gapless: true,
            normalisation: false,
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

/// Spotify client ids are 32 hex characters.
pub fn valid_client_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Config {
    /// Load the config file. `client_id` is required for Web API access; when it is missing
    /// and stdin is a terminal, ask for it and save it.
    pub fn load(paths: &Paths) -> Result<Self> {
        if !paths.config_file.exists() {
            Config::default().save(&paths.config_file)?;
        }
        let text = std::fs::read_to_string(&paths.config_file)?;
        let mut cfg: Config = toml::from_str(&text)
            .with_context(|| format!("invalid config {}", paths.config_file.display()))?;
        if cfg.client_id.trim().is_empty() {
            if !std::io::stdin().is_terminal() {
                bail!(
                    "Set `client_id` in {}\n\
                     1. Create an app at https://developer.spotify.com/dashboard\n\
                     2. Add redirect URI: {REDIRECT_URI}\n\
                     3. Copy its Client ID into the config file and re-run.",
                    paths.config_file.display()
                );
            }
            cfg.client_id = prompt_client_id()?;
            cfg.save(&paths.config_file)?;
            println!("Saved to {}\n", paths.config_file.display());
        }
        Ok(cfg)
    }

    /// Write the config atomically (temp file + rename).
    pub fn save(&self, file: &Path) -> Result<()> {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = file.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)
            .and_then(|()| std::fs::rename(&tmp, file))
            .with_context(|| format!("writing {}", file.display()))
    }

    /// `bitrate` snapped to one Spotify serves (96, 160 or 320 kbps).
    pub fn bitrate_kbps(&self) -> u16 {
        match self.bitrate {
            0..=96 => 96,
            97..=160 => 160,
            _ => 320,
        }
    }
}

fn prompt_client_id() -> Result<String> {
    println!(
        "spotify-tui needs the Client ID of your own Spotify app:\n  \
         1. Create an app at https://developer.spotify.com/dashboard\n  \
         2. Add redirect URI: {REDIRECT_URI}\n  \
         3. Paste its Client ID below.\n"
    );
    let stdin = std::io::stdin();
    loop {
        print!("Client ID: ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            bail!("no Client ID entered");
        }
        let id = line.trim();
        if valid_client_id(id) {
            return Ok(id.to_string());
        }
        println!("That isn't a Client ID (expected 32 hex characters, as shown on the app's dashboard page).");
    }
}
