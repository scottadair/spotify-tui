//! Small JSON disk cache for listings (playlists, track lists, browse pages).
//! I/O and (de)serialization run on the blocking pool so large lists never stall the UI.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct Cache {
    dir: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Envelope<T> {
    saved_at: u64,
    data: T,
}

pub struct Cached<T> {
    pub data: T,
    pub age: Duration,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Cache {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, key: &str) -> PathBuf {
        let safe: String = key
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        self.dir.join(format!("{safe}.json"))
    }

    /// Cached value of any age; the caller decides whether it is fresh enough.
    pub async fn read<T: DeserializeOwned + Send + 'static>(&self, key: &str) -> Option<Cached<T>> {
        let path = self.path(key);
        tokio::task::spawn_blocking(move || {
            let bytes = std::fs::read(path).ok()?;
            let env: Envelope<T> = serde_json::from_slice(&bytes).ok()?;
            let age = Duration::from_secs(now_secs().saturating_sub(env.saved_at));
            Some(Cached { data: env.data, age })
        })
        .await
        .ok()
        .flatten()
    }

    /// Best-effort write (atomic via rename); failures only cost a future cache miss.
    pub async fn write<T: Serialize + Send + 'static>(&self, key: &str, data: T) {
        let path = self.path(key);
        let dir = self.dir.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let env = Envelope { saved_at: now_secs(), data };
            let Ok(bytes) = serde_json::to_vec(&env) else { return };
            if std::fs::create_dir_all(&dir).is_err() {
                return;
            }
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip_and_corrupt_files_are_misses() {
        let dir = std::env::temp_dir().join(format!("spotify-tui-cache-test-{}", std::process::id()));
        let cache = Cache::new(dir.clone());
        assert!(cache.read::<Vec<String>>("a/b key").await.is_none());

        cache.write("a/b key", vec!["x".to_string()]).await;
        let got = cache.read::<Vec<String>>("a/b key").await.unwrap();
        assert_eq!(got.data, ["x"]);
        assert!(got.age < Duration::from_secs(5));

        std::fs::write(dir.join("bad.json"), b"{not json").unwrap();
        assert!(cache.read::<Vec<String>>("bad").await.is_none());
        std::fs::remove_dir_all(dir).ok();
    }
}
