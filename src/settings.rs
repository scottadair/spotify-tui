//! Settings overlay. Every change is written to `config.toml` straight away; all of these are
//! read at startup, so they take effect on the next launch (rows that differ from the running
//! config say so).

use crate::config::{Config, valid_client_id};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Bitrate,
    Normalisation,
    Gapless,
    DeviceName,
    ClientId,
}

pub const FIELDS: [Field; 5] =
    [Field::Bitrate, Field::Normalisation, Field::Gapless, Field::DeviceName, Field::ClientId];

const BITRATES: [u16; 3] = [96, 160, 320];

impl Field {
    pub fn label(self) -> &'static str {
        match self {
            Field::Bitrate => "Audio quality",
            Field::Normalisation => "Volume normalisation",
            Field::Gapless => "Gapless playback",
            Field::DeviceName => "Device name",
            Field::ClientId => "Client ID",
        }
    }

    /// One line shown under the list for the selected field.
    pub fn description(self) -> &'static str {
        match self {
            Field::Bitrate => "Streaming bitrate. Higher sounds better and uses more data.",
            Field::Normalisation => "Evens out loudness differences between tracks.",
            Field::Gapless => "Plays consecutive tracks with no silence between them.",
            Field::DeviceName => "Name shown in Spotify Connect device lists.",
            Field::ClientId => "Your app's Client ID from developer.spotify.com/dashboard.",
        }
    }

    pub fn value(self, c: &Config) -> String {
        let on_off = |b: bool| if b { "on" } else { "off" }.to_string();
        match self {
            Field::Bitrate => format!("{} kbps", c.bitrate_kbps()),
            Field::Normalisation => on_off(c.normalisation),
            Field::Gapless => on_off(c.gapless),
            Field::DeviceName => c.device_name.clone(),
            Field::ClientId => c.client_id.clone(),
        }
    }

    pub fn is_text(self) -> bool {
        matches!(self, Field::DeviceName | Field::ClientId)
    }
}

pub enum Outcome {
    Stay,
    Close,
}

pub struct Settings {
    pub open: bool,
    /// As saved on disk.
    pub config: Config,
    /// What this process started with.
    running: Config,
    file: PathBuf,
    pub selected: usize,
    /// Text being typed into the selected text field.
    pub input: Option<String>,
    /// Last validation or save error.
    pub error: Option<String>,
}

impl Settings {
    pub fn new(config: Config, file: PathBuf) -> Self {
        Self { open: false, running: config.clone(), config, file, selected: 0, input: None, error: None }
    }

    pub fn field(&self) -> Field {
        FIELDS[self.selected]
    }

    /// Saved but not yet in effect.
    pub fn pending(&self, f: Field) -> bool {
        f.value(&self.config) != f.value(&self.running)
    }

    pub fn on_key(&mut self, k: KeyEvent) -> Outcome {
        if let Some(input) = &mut self.input {
            match k.code {
                KeyCode::Esc => {
                    self.input = None;
                    self.error = None;
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => input.push(c),
                KeyCode::Enter => self.commit(),
                _ => {}
            }
            return Outcome::Stay;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Char(',' | 'q') => {
                self.error = None;
                return Outcome::Close;
            }
            KeyCode::Char('j') | KeyCode::Down => self.selected = (self.selected + 1).min(FIELDS.len() - 1),
            KeyCode::Char('k') | KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Enter if self.field().is_text() => {
                self.error = None;
                self.input = Some(self.field().value(&self.config));
            }
            KeyCode::Enter | KeyCode::Char(' ' | 'l') | KeyCode::Right => self.change(true),
            KeyCode::Char('h') | KeyCode::Left => self.change(false),
            _ => {}
        }
        Outcome::Stay
    }

    fn change(&mut self, up: bool) {
        let mut next = self.config.clone();
        match self.field() {
            Field::Bitrate => {
                let i = BITRATES.iter().position(|&b| b == next.bitrate_kbps()).unwrap_or(0);
                let n = BITRATES.len();
                next.bitrate = BITRATES[if up { (i + 1) % n } else { (i + n - 1) % n }];
            }
            Field::Normalisation => next.normalisation = !next.normalisation,
            Field::Gapless => next.gapless = !next.gapless,
            Field::DeviceName | Field::ClientId => return,
        }
        self.save(next);
    }

    fn commit(&mut self) {
        let Some(input) = &self.input else { return };
        let text = input.trim().to_string();
        let mut next = self.config.clone();
        match self.field() {
            Field::ClientId if !valid_client_id(&text) => {
                self.error = Some("A Client ID is 32 hex characters".into());
                return;
            }
            Field::ClientId => next.client_id = text,
            Field::DeviceName if text.is_empty() => {
                self.error = Some("Device name can't be empty".into());
                return;
            }
            Field::DeviceName => next.device_name = text,
            _ => return,
        }
        self.input = None;
        self.save(next);
    }

    fn save(&mut self, next: Config) {
        match next.save(&self.file) {
            Ok(()) => {
                self.config = next;
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(s: &mut Settings, text: &str) {
        for c in text.chars() {
            s.on_key(key(KeyCode::Char(c)));
        }
    }

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spotify-tui-settings-{name}-{}", std::process::id()));
        dir.join("config.toml")
    }

    fn on_disk(file: &PathBuf) -> Config {
        toml::from_str(&std::fs::read_to_string(file).unwrap()).unwrap()
    }

    #[test]
    fn client_id_is_validated_before_saving() {
        let file = temp_file("client-id");
        let mut s = Settings::new(Config::default(), file.clone());
        s.selected = FIELDS.iter().position(|&f| f == Field::ClientId).unwrap();

        s.on_key(key(KeyCode::Enter));
        type_text(&mut s, "not-an-id");
        s.on_key(key(KeyCode::Enter));
        assert!(s.error.is_some());
        assert!(s.input.is_some(), "stays in the editor to fix the typo");
        assert!(!file.exists());

        s.input = Some(String::new());
        type_text(&mut s, " 0123456789abcdef0123456789ABCDEF ");
        s.on_key(key(KeyCode::Enter));
        assert!(s.error.is_none() && s.input.is_none());
        assert_eq!(on_disk(&file).client_id, "0123456789abcdef0123456789ABCDEF");
        assert!(s.pending(Field::ClientId));
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn bitrate_cycles_both_ways_from_off_grid_values() {
        let file = temp_file("bitrate");
        // 200 runs as 320; stepping must move relative to that, not get stuck.
        let mut s = Settings::new(Config { bitrate: 200, ..Config::default() }, file.clone());
        s.on_key(key(KeyCode::Right));
        assert_eq!(s.config.bitrate, 96);
        s.on_key(key(KeyCode::Left));
        s.on_key(key(KeyCode::Left));
        assert_eq!(on_disk(&file).bitrate, 160);
        assert!(s.pending(Field::Bitrate));
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }
}
