//! On-disk cache for OIDC tokens obtained via `tzctl auth login`.
//!
//! The cache lives under the OS config directory (e.g.
//! `~/.config/tzctl/credentials.json`) with file mode `0600`. Entries are keyed
//! by `(api_endpoint, issuer)` so multiple platforms coexist.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A single cached credential entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedToken {
    /// The OIDC ID token.
    pub id_token: String,
    /// A refresh token, when one was issued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Token expiry as a Unix timestamp (seconds), if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// The issuer the token was obtained from.
    pub issuer: String,
}

/// A cached workspace-scoped `user_key`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedUserKey {
    /// The workspace-scoped key sent as `X-Tenzir-UserKey`.
    pub user_key: String,
    /// Expiry as a Unix timestamp (seconds), if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

/// The full credentials file: maps of cache key → entry.
#[derive(Debug, Default, Serialize, Deserialize)]
struct CredentialStore {
    #[serde(default)]
    tokens: BTreeMap<String, CachedToken>,
    #[serde(default)]
    user_keys: BTreeMap<String, CachedUserKey>,
}

/// Compute the cache key for a platform endpoint and issuer.
fn cache_key(api_endpoint: &str, issuer: &str) -> String {
    format!("{api_endpoint}|{issuer}")
}

/// The default credentials file path under the OS config dir.
///
/// Falls back to `./.tzctl/credentials.json` if no config dir is available.
pub fn default_path() -> PathBuf {
    if let Some(dirs) = directories::ProjectDirs::from("com", "tenzir", "tzctl") {
        dirs.config_dir().join("credentials.json")
    } else {
        PathBuf::from(".tzctl/credentials.json")
    }
}

/// A handle to the credentials cache at a specific path.
#[derive(Debug, Clone)]
pub struct Cache {
    path: PathBuf,
}

impl Default for Cache {
    fn default() -> Self {
        Self::new(default_path())
    }
}

impl Cache {
    /// Create a cache backed by `path` (the file need not exist yet).
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The backing file path.
    #[allow(dead_code)] // used by tests and the client stage.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the credential store, returning an empty one if the file is absent.
    fn load_store(&self) -> Result<CredentialStore> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| {
                Error::Auth(format!(
                    "cannot parse credentials cache {}: {e}",
                    self.path.display()
                ))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(CredentialStore::default()),
            Err(e) => Err(Error::Auth(format!(
                "cannot read credentials cache {}: {e}",
                self.path.display()
            ))),
        }
    }

    /// Persist the credential store with `0600` permissions.
    fn save_store(&self, store: &CredentialStore) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::Auth(format!("cannot create cache dir {}: {e}", parent.display()))
            })?;
        }
        let text = serde_json::to_string_pretty(store)
            .map_err(|e| Error::Auth(format!("cannot serialize credentials: {e}")))?;
        std::fs::write(&self.path, text).map_err(|e| {
            Error::Auth(format!(
                "cannot write credentials cache {}: {e}",
                self.path.display()
            ))
        })?;
        set_owner_only(&self.path)?;
        Ok(())
    }

    /// Retrieve the cached token for `(api_endpoint, issuer)`, if any.
    #[allow(dead_code)] // consumed by the token-resolution path.
    pub fn get(&self, api_endpoint: &str, issuer: &str) -> Result<Option<CachedToken>> {
        let store = self.load_store()?;
        Ok(store.tokens.get(&cache_key(api_endpoint, issuer)).cloned())
    }

    /// Store a token for `(api_endpoint, issuer)`.
    pub fn put(&self, api_endpoint: &str, token: CachedToken) -> Result<()> {
        let mut store = self.load_store()?;
        let key = cache_key(api_endpoint, &token.issuer);
        store.tokens.insert(key, token);
        self.save_store(&store)
    }

    /// Remove the token for `(api_endpoint, issuer)`.
    ///
    /// Returns `true` if an entry was removed.
    pub fn remove(&self, api_endpoint: &str, issuer: &str) -> Result<bool> {
        let mut store = self.load_store()?;
        let removed = store
            .tokens
            .remove(&cache_key(api_endpoint, issuer))
            .is_some();
        let prefix = format!("{api_endpoint}|");
        let before = store.user_keys.len();
        store.user_keys.retain(|k, _| !k.starts_with(&prefix));
        let keys_removed = store.user_keys.len() != before;
        if removed || keys_removed {
            self.save_store(&store)?;
        }
        Ok(removed)
    }

    /// Retrieve the cached `user_key` for `(api_endpoint, tenant_id)`, if any.
    #[allow(dead_code)] // consumed by the session layer.
    pub fn get_user_key(
        &self,
        api_endpoint: &str,
        tenant_id: &str,
    ) -> Result<Option<CachedUserKey>> {
        let store = self.load_store()?;
        Ok(store
            .user_keys
            .get(&cache_key(api_endpoint, tenant_id))
            .cloned())
    }

    /// Store a `user_key` for `(api_endpoint, tenant_id)`.
    #[allow(dead_code)] // consumed by the session layer.
    pub fn put_user_key(
        &self,
        api_endpoint: &str,
        tenant_id: &str,
        key: CachedUserKey,
    ) -> Result<()> {
        let mut store = self.load_store()?;
        store
            .user_keys
            .insert(cache_key(api_endpoint, tenant_id), key);
        self.save_store(&store)
    }

    /// Remove the cached `user_key` for `(api_endpoint, tenant_id)`.
    #[allow(dead_code)] // consumed by the session layer.
    pub fn remove_user_key(&self, api_endpoint: &str, tenant_id: &str) -> Result<bool> {
        let mut store = self.load_store()?;
        let removed = store
            .user_keys
            .remove(&cache_key(api_endpoint, tenant_id))
            .is_some();
        if removed {
            self.save_store(&store)?;
        }
        Ok(removed)
    }
}

/// Restrict a file to owner read/write (`0600`) on Unix; a no-op elsewhere.
#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms)
        .map_err(|e| Error::Auth(format!("cannot set permissions on {}: {e}", path.display())))
}

/// Non-Unix platforms: permission hardening is a no-op.
#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(id: &str) -> CachedToken {
        CachedToken {
            id_token: id.to_string(),
            refresh_token: None,
            expires_at: Some(9_999_999_999),
            issuer: "https://issuer.test/".to_string(),
        }
    }

    #[test]
    fn round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = Cache::new(tmp.path().join("credentials.json"));
        let endpoint = "https://api.test";
        assert!(
            cache
                .get(endpoint, "https://issuer.test/")
                .unwrap()
                .is_none()
        );

        cache.put(endpoint, token("abc")).unwrap();
        let got = cache
            .get(endpoint, "https://issuer.test/")
            .unwrap()
            .unwrap();
        assert_eq!(got.id_token, "abc");

        assert!(cache.remove(endpoint, "https://issuer.test/").unwrap());
        assert!(
            cache
                .get(endpoint, "https://issuer.test/")
                .unwrap()
                .is_none()
        );
        assert!(!cache.remove(endpoint, "https://issuer.test/").unwrap());
    }

    #[test]
    fn keys_are_isolated_per_endpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = Cache::new(tmp.path().join("credentials.json"));
        cache.put("https://a.test", token("token-a")).unwrap();
        cache.put("https://b.test", token("token-b")).unwrap();
        assert_eq!(
            cache
                .get("https://a.test", "https://issuer.test/")
                .unwrap()
                .unwrap()
                .id_token,
            "token-a"
        );
        assert_eq!(
            cache
                .get("https://b.test", "https://issuer.test/")
                .unwrap()
                .unwrap()
                .id_token,
            "token-b"
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let cache = Cache::new(tmp.path().join("credentials.json"));
        cache.put("https://a.test", token("x")).unwrap();
        let mode = std::fs::metadata(cache.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
