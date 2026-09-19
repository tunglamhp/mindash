//! Shared application state.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use mindash_core::Config;
use tokio::sync::RwLock;

/// Cached upstream payload with its fetch time, so a burst of dashboard clients
/// does not translate into a burst of requests to a public API.
pub struct Cached {
    pub at: Instant,
    pub value: serde_json::Value,
}

pub struct AppState {
    pub data_dir: PathBuf,
    pub static_dir: PathBuf,
    config: RwLock<Config>,
    /// `None` means auth is disabled.
    password_hash: RwLock<Option<String>>,
    session_secret: RwLock<Option<Vec<u8>>>,
    pub cache: RwLock<std::collections::HashMap<String, Cached>>,
    pub http: reqwest::Client,
    pub started: Instant,
    /// Bumped on every config change; SSE clients use it to skip no-op pushes.
    pub revision: AtomicU64,
    /// Serialises the persist step.
    ///
    /// `update_config` mutates and serialises under the config lock, then
    /// releases it before touching the filesystem. Without this second lock,
    /// concurrent writers race on the same temp path: one renames it away and
    /// the next `rename` fails with "file not found". Holding the config lock
    /// across the write would avoid that but would block every reader for the
    /// duration of a disk write.
    write_lock: tokio::sync::Mutex<()>,
}

impl AppState {
    pub async fn new(data_dir: PathBuf, static_dir: PathBuf) -> std::io::Result<Self> {
        tokio::fs::create_dir_all(&data_dir).await?;

        let config_path = data_dir.join("config.json");
        let raw = tokio::fs::read(&config_path).await.ok();
        let (config, warning) = Config::load_or_default(raw.as_deref());
        if let Some(w) = warning {
            tracing::warn!("{w}");
            // Keep the bad file for inspection instead of silently overwriting.
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let backup = data_dir.join(format!("config.json.broken-{stamp}"));
            let _ = tokio::fs::copy(&config_path, &backup).await;
            tracing::warn!("kept a copy at {}", backup.display());
        }

        let password_hash = read_optional(&data_dir.join(".password")).await;
        let session_secret = read_optional(&data_dir.join(".session_secret"))
            .await
            .map(|s| s.trim().as_bytes().to_vec());

        let http = reqwest::Client::builder()
            .user_agent(concat!("MinDash/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        let state = Self {
            data_dir,
            static_dir,
            config: RwLock::new(config),
            password_hash: RwLock::new(password_hash),
            session_secret: RwLock::new(session_secret),
            cache: RwLock::new(Default::default()),
            http,
            started: Instant::now(),
            revision: AtomicU64::new(1),
            write_lock: tokio::sync::Mutex::new(()),
        };
        // A first run has no secrets; generate and persist them.
        state.ensure_session_secret().await;
        Ok(state)
    }

    pub fn auth_enabled(&self) -> bool {
        // A read lock on a bool-ish value; the blocking variant is fine here
        // because the lock is never held across an await.
        self.password_hash
            .try_read()
            .map(|g| g.is_some())
            .unwrap_or(false)
    }

    pub async fn set_password(&self, password: &str) -> std::io::Result<()> {
        let hash = crate::auth::hash_password(password);
        let path = self.data_dir.join(".password");
        tokio::fs::write(&path, &hash).await?;
        restrict(&path);
        *self.password_hash.write().await = Some(hash);
        Ok(())
    }

    pub async fn verify_password(&self, password: &str) -> bool {
        let guard = self.password_hash.read().await;
        match guard.as_ref() {
            None => true,
            Some(hash) => crate::auth::verify_password(password, hash),
        }
    }

    async fn ensure_session_secret(&self) {
        let mut guard = self.session_secret.write().await;
        if guard.is_none() {
            let secret = crate::auth::random_secret().into_bytes();
            let path = self.data_dir.join(".session_secret");
            if let Err(e) = tokio::fs::write(&path, &secret).await {
                tracing::error!("could not persist session secret: {e}");
            }
            restrict(&path);
            *guard = Some(secret);
        }
    }

    pub async fn issue_session(&self) -> String {
        let guard = self.session_secret.read().await;
        let secret = guard.clone().unwrap_or_default();
        crate::auth::issue_session(&secret)
    }

    pub fn verify_session(&self, token: &str) -> bool {
        let Ok(guard) = self.session_secret.try_read() else {
            return false;
        };
        let Some(secret) = guard.as_ref() else {
            return false;
        };
        crate::auth::verify_session(token, secret)
    }

    /// A snapshot of the config. Cheap: the config is small and this is a read.
    pub async fn config(&self) -> Config {
        self.config.read().await.clone()
    }

    /// A specific section of the config, without cloning the rest.
    pub async fn section_order(&self) -> Vec<String> {
        self.config.read().await.settings.section_order.clone()
    }

    /// Mutate the config and persist it.
    ///
    /// The write is atomic: serialise, write a sibling temp file, then rename
    /// over the target. The Python version wrote in place, so a crash or a full
    /// disk mid-write left a truncated file that stopped the server booting —
    /// which is exactly the bug the recovery path exists to clean up after.
    pub async fn update_config<F>(&self, f: F) -> std::io::Result<()>
    where
        F: FnOnce(&mut Config),
    {
        // Mutate and serialise while holding the config lock, then drop it before
        // doing any I/O so readers are not blocked by a disk write.
        let snapshot = {
            let mut guard = self.config.write().await;
            f(&mut guard);
            serde_json::to_vec_pretty(&*guard).map_err(|e| std::io::Error::other(e.to_string()))?
        };

        // One writer at a time for the temp-file-then-rename sequence.
        let _writing = self.write_lock.lock().await;

        let path = self.data_dir.join("config.json");
        let tmp = self.data_dir.join("config.json.tmp");
        tokio::fs::write(&tmp, &snapshot).await?;
        tokio::fs::rename(&tmp, &path).await?;
        self.revision.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    /// Read-through cache with a TTL, returning stale data if the refresh fails.
    pub async fn cached<F, Fut>(&self, key: &str, ttl: Duration, fetch: F) -> serde_json::Value
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<serde_json::Value, String>>,
    {
        {
            let cache = self.cache.read().await;
            if let Some(hit) = cache.get(key) {
                if hit.at.elapsed() < ttl {
                    return hit.value.clone();
                }
            }
        }

        match fetch().await {
            Ok(value) => {
                self.cache.write().await.insert(
                    key.to_string(),
                    Cached {
                        at: Instant::now(),
                        value: value.clone(),
                    },
                );
                value
            }
            Err(e) => {
                let cache = self.cache.read().await;
                if let Some(hit) = cache.get(key) {
                    let mut stale = hit.value.clone();
                    if let Some(o) = stale.as_object_mut() {
                        o.insert("stale".into(), serde_json::Value::Bool(true));
                    }
                    tracing::warn!("{key}: {e}; serving cached data");
                    return stale;
                }
                serde_json::json!({ "error": e })
            }
        }
    }
}

async fn read_optional(path: &Path) -> Option<String> {
    tokio::fs::read_to_string(path).await.ok()
}

/// Make a file readable only by its owner, where the platform supports it.
///
/// The password hash and the session secret are both credentials. A default
/// `write` honours the umask, which on a permissive system leaves them readable
/// by every local user. Best effort: a failure here is not worth refusing to
/// start over, so it is logged rather than propagated.
fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            tracing::warn!("could not restrict permissions on {}: {e}", path.display());
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn state() -> (std::sync::Arc<AppState>, tempdir::Dir) {
        let dir = tempdir::Dir::new();
        let s = AppState::new(dir.path().to_path_buf(), dir.path().to_path_buf())
            .await
            .unwrap();
        (std::sync::Arc::new(s), dir)
    }

    /// Minimal temp directory so the tests do not need an extra crate.
    mod tempdir {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        pub struct Dir(PathBuf);
        impl Dir {
            pub fn new() -> Self {
                let p = std::env::temp_dir().join(format!(
                    "mindash-test-{}-{}",
                    std::process::id(),
                    N.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::create_dir_all(&p).unwrap();
                Self(p)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[tokio::test]
    async fn fresh_state_has_a_host_and_a_secret() {
        let (s, _d) = state().await;
        let cfg = s.config().await;
        assert!(cfg.devices.iter().any(|d| d.is_host));
        assert!(!s.auth_enabled(), "auth is off until a password is set");
        let token = s.issue_session().await;
        assert!(s.verify_session(&token));
    }

    #[tokio::test]
    async fn password_round_trips() {
        let (s, _d) = state().await;
        s.set_password("hunter2").await.unwrap();
        assert!(s.auth_enabled());
        assert!(s.verify_password("hunter2").await);
        assert!(!s.verify_password("hunter3").await);
    }

    #[tokio::test]
    async fn concurrent_updates_all_land() {
        let (s, _d) = state().await;
        let mut handles = Vec::new();
        for i in 0..32 {
            let s = s.clone();
            handles.push(tokio::spawn(async move {
                s.update_config(move |c| {
                    c.links.push(mindash_core::config::Link {
                        id: format!("l{i}"),
                        ..Default::default()
                    });
                })
                .await
                .unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(s.config().await.links.len(), 32);
    }

    #[tokio::test]
    async fn revision_advances_only_on_write() {
        let (s, _d) = state().await;
        let r0 = s.revision();
        s.update_config(|c| c.onboarding_done = true).await.unwrap();
        assert!(s.revision() > r0);
    }

    #[tokio::test]
    async fn corrupt_config_is_reported_and_backed_up() {
        let dir = tempdir::Dir::new();
        std::fs::write(dir.path().join("config.json"), b"{\"devices\":[{\"id\":").unwrap();
        let s = AppState::new(dir.path().to_path_buf(), dir.path().to_path_buf())
            .await
            .unwrap();
        assert!(s.config().await.devices.iter().any(|d| d.is_host));
        let backups: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains("broken"))
            .collect();
        assert_eq!(backups.len(), 1, "the bad file is preserved for inspection");
    }
}
