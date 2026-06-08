//! Authentication: OIDC flows, id-token cache, and token resolution.
//!
//! This stage obtains and caches an OIDC `id_token`. The `id_token` →
//! `user_key` exchange lives in the platform-client stage.

pub mod cache;
pub mod client_credentials;
pub mod device;
pub mod oidc;
pub mod token;

use std::path::PathBuf;

use owo_colors::OwoColorize;

use crate::config::ResolvedConfig;
use crate::error::{Error, Result};
use cache::{Cache, CachedToken};
use oidc::OidcSettings;
use token::{Claims, TokenResponse};

/// Which login flow to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginMode {
    /// Force the interactive device-code flow.
    Interactive,
    /// Force the non-interactive client-credentials flow.
    NonInteractive,
    /// Auto-select based on whether a client secret is configured.
    Auto,
}

/// Pre-supplied token sources, resolved from CLI/config/env.
// Fields are read by the token-resolution path, wired up in the client stage.
#[allow(dead_code)]
#[derive(Debug, Default, Clone)]
pub struct TokenSources {
    /// An explicit token from `--token`, `[auth].id_token`, or env.
    pub explicit_token: Option<String>,
    /// A path to a file containing the id token (`[auth].token_file`).
    pub token_file: Option<String>,
}

impl TokenSources {
    /// Build token sources from resolved config and an optional `--token` flag.
    pub fn from_config(config: &ResolvedConfig, cli_token: Option<String>) -> Self {
        Self {
            explicit_token: cli_token.or_else(|| config.id_token.clone()),
            token_file: config.token_file.clone(),
        }
    }
}

/// Drives the OIDC flows and token cache for a single platform endpoint.
pub struct Authenticator {
    http: reqwest::Client,
    api_endpoint: String,
    settings: OidcSettings,
    cache: Cache,
    #[allow(dead_code)] // read by the token-resolution path (client stage).
    sources: TokenSources,
}

/// Expand a leading `~` in a path to the user's home directory.
#[allow(dead_code)] // used by the token-resolution path (client stage).
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(dirs) = directories::UserDirs::new()
    {
        return dirs.home_dir().join(rest);
    }
    PathBuf::from(path)
}

impl Authenticator {
    /// Construct an authenticator from resolved config and OIDC env settings.
    pub fn new(config: &ResolvedConfig, sources: TokenSources) -> Result<Self> {
        // OIDC settings come from the `[platform.oidc]` config table, falling
        // back to built-in defaults for any unset values.
        let settings = OidcSettings::from_config(config);
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| Error::Auth(format!("cannot build HTTP client: {e}")))?;
        Ok(Self {
            http,
            api_endpoint: config.api_endpoint.clone(),
            settings,
            cache: Cache::default(),
            sources,
        })
    }

    /// Override the credential cache (used in tests).
    #[cfg(test)]
    pub fn with_cache(mut self, cache: Cache) -> Self {
        self.cache = cache;
        self
    }

    /// Override OIDC settings (used in tests).
    #[cfg(test)]
    pub fn with_settings(mut self, settings: OidcSettings) -> Self {
        self.settings = settings;
        self
    }

    /// Perform a login, cache the resulting token, and return its claims.
    ///
    /// `mode` selects the flow; `notify` presents the device-code prompt.
    pub async fn login(
        &self,
        mode: LoginMode,
        notify: impl FnOnce(&str, &str, Option<&str>),
    ) -> Result<Claims> {
        let discovery = oidc::discover(&self.http, &self.settings).await?;
        let use_client_credentials = match mode {
            LoginMode::NonInteractive => true,
            LoginMode::Interactive => false,
            LoginMode::Auto => self.settings.client_secret.is_some(),
        };
        if mode == LoginMode::NonInteractive && self.settings.client_secret.is_none() {
            return Err(Error::Auth(
                "non-interactive login requires a client secret in [platform.oidc]".to_string(),
            ));
        }
        let response = if use_client_credentials {
            client_credentials::run(&self.http, &self.settings, &discovery).await?
        } else {
            device::run(&self.http, &self.settings, &discovery, notify).await?
        };
        let id_token = response
            .id_token
            .clone()
            .ok_or_else(|| Error::Auth("the provider did not return an id_token".to_string()))?;
        let claims = token::decode_claims(&id_token).unwrap_or_default();
        self.cache.put(
            &self.api_endpoint,
            self.to_cached(&response, &id_token, &claims),
        )?;
        Ok(claims)
    }

    /// Build a cache entry from a token response and decoded claims.
    fn to_cached(&self, resp: &TokenResponse, id_token: &str, claims: &Claims) -> CachedToken {
        let expires_at = claims.exp.or_else(|| {
            resp.expires_in
                .map(|secs| time::OffsetDateTime::now_utc().unix_timestamp() + secs)
        });
        CachedToken {
            id_token: id_token.to_string(),
            refresh_token: resp.refresh_token.clone(),
            expires_at,
            issuer: self.settings.issuer.clone(),
        }
    }

    /// Remove cached credentials for this endpoint/issuer.
    ///
    /// Returns `true` if an entry was removed.
    pub fn logout(&self) -> Result<bool> {
        self.cache.remove(&self.api_endpoint, &self.settings.issuer)
    }

    /// Resolve a usable `id_token`, applying the documented precedence.
    ///
    /// 1. Explicit token (`--token` / `[auth].id_token` / env).
    /// 2. `[auth].token_file`.
    /// 3. A valid (non-expired) cached token.
    /// 4. Mint via client-credentials when a secret is configured.
    /// 5. Otherwise error with a hint to run `tz auth login`.
    #[allow(dead_code)] // wired into commands in the client stage.
    pub async fn load_id_token(&self) -> Result<String> {
        if let Some(tok) = &self.sources.explicit_token {
            return Ok(tok.clone());
        }
        if let Some(path) = &self.sources.token_file {
            let expanded = expand_tilde(path);
            let tok = std::fs::read_to_string(&expanded).map_err(|e| {
                Error::Auth(format!(
                    "cannot read token_file {}: {e}",
                    expanded.display()
                ))
            })?;
            return Ok(tok.trim().to_string());
        }
        if let Some(cached) = self.cache.get(&self.api_endpoint, &self.settings.issuer)?
            && let Some(valid) = self.usable_cached(cached).await?
        {
            return Ok(valid);
        }
        if self.settings.client_secret.is_some() {
            let discovery = oidc::discover(&self.http, &self.settings).await?;
            let response = client_credentials::run(&self.http, &self.settings, &discovery).await?;
            let id_token = response.id_token.clone().ok_or_else(|| {
                Error::Auth("the provider did not return an id_token".to_string())
            })?;
            let claims = token::decode_claims(&id_token).unwrap_or_default();
            self.cache.put(
                &self.api_endpoint,
                self.to_cached(&response, &id_token, &claims),
            )?;
            return Ok(id_token);
        }
        Err(Error::Auth("not logged in".to_string()))
    }

    /// Return the cached token if still valid, refreshing it if possible.
    #[allow(dead_code)] // part of the token-resolution path.
    async fn usable_cached(&self, cached: CachedToken) -> Result<Option<String>> {
        let expired = cached
            .expires_at
            .is_some_and(|exp| token::is_expired(exp, 60));
        if !expired {
            return Ok(Some(cached.id_token));
        }
        if let Some(refresh) = &cached.refresh_token
            && let Ok(refreshed) = self.refresh(refresh).await
        {
            return Ok(Some(refreshed));
        }
        Ok(None)
    }

    /// Exchange a refresh token for a fresh id token and update the cache.
    #[allow(dead_code)] // part of the token-resolution path.
    async fn refresh(&self, refresh_token: &str) -> Result<String> {
        let discovery = oidc::discover(&self.http, &self.settings).await?;
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", self.settings.client_id.as_str()),
        ];
        let resp = self
            .http
            .post(&discovery.token_endpoint)
            .form(&form)
            .send()
            .await
            .map_err(|e| Error::Auth(format!("token refresh failed: {e}")))?
            .error_for_status()
            .map_err(|e| Error::Auth(format!("token refresh rejected: {e}")))?
            .json::<TokenResponse>()
            .await
            .map_err(|e| Error::Auth(format!("cannot parse refresh response: {e}")))?;
        let id_token = response_id_token(&resp)?;
        let claims = token::decode_claims(&id_token).unwrap_or_default();
        let mut entry = self.to_cached(&resp, &id_token, &claims);
        // Preserve the refresh token if the provider didn't return a new one.
        if entry.refresh_token.is_none() {
            entry.refresh_token = Some(refresh_token.to_string());
        }
        self.cache.put(&self.api_endpoint, entry)?;
        Ok(id_token)
    }

    /// The issuer this authenticator targets.
    #[allow(dead_code)] // used by the client stage.
    pub fn issuer(&self) -> &str {
        &self.settings.issuer
    }
}

/// Extract the id token from a response, erroring if absent.
#[allow(dead_code)] // used by the refresh path.
fn response_id_token(resp: &TokenResponse) -> Result<String> {
    resp.id_token
        .clone()
        .ok_or_else(|| Error::Auth("the provider did not return an id_token".to_string()))
}

/// Default presenter for the device-code prompt (writes to stderr).
pub fn print_device_prompt(uri: &str, code: &str, complete: Option<&str>) {
    eprintln!("To authenticate, open the following URL in your browser:");
    if let Some(c) = complete {
        eprintln!("  {}", c.bold());
        eprintln!("(or visit {uri} and enter code {})", code.bold());
    } else {
        eprintln!("  {}", uri.bold());
        eprintln!("and enter the code {}", code.bold());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_API_ENDPOINT;

    fn config() -> ResolvedConfig {
        ResolvedConfig {
            api_endpoint: DEFAULT_API_ENDPOINT.to_string(),
            oidc_issuer: None,
            client_id: None,
            client_secret: None,
            oidc_audience: None,
            oidc_scope: None,
            workspace: None,
            node: None,
            pipelines_glob: "p".to_string(),
            default_state: None,
            id_token: None,
            token_file: None,
            config_dir: None,
            project_root: std::path::PathBuf::from("."),
        }
    }

    #[tokio::test]
    async fn explicit_token_wins() {
        let mut cfg = config();
        cfg.id_token = Some("inline-token".to_string());
        let sources = TokenSources::from_config(&cfg, None);
        let auth = Authenticator::new(&cfg, sources).unwrap();
        assert_eq!(auth.load_id_token().await.unwrap(), "inline-token");
    }

    #[tokio::test]
    async fn cli_token_overrides_config() {
        let mut cfg = config();
        cfg.id_token = Some("inline-token".to_string());
        let sources = TokenSources::from_config(&cfg, Some("flag-token".to_string()));
        let auth = Authenticator::new(&cfg, sources).unwrap();
        assert_eq!(auth.load_id_token().await.unwrap(), "flag-token");
    }

    #[tokio::test]
    async fn token_file_is_read() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("token");
        std::fs::write(&file, "  file-token\n").unwrap();
        let mut cfg = config();
        cfg.token_file = Some(file.to_string_lossy().to_string());
        let sources = TokenSources::from_config(&cfg, None);
        let auth = Authenticator::new(&cfg, sources).unwrap();
        assert_eq!(auth.load_id_token().await.unwrap(), "file-token");
    }

    #[tokio::test]
    async fn errors_when_not_logged_in() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = config();
        let sources = TokenSources::from_config(&cfg, None);
        let auth = Authenticator::new(&cfg, sources)
            .unwrap()
            .with_cache(Cache::new(tmp.path().join("credentials.json")));
        let err = auth.load_id_token().await.unwrap_err();
        assert!(matches!(err, Error::Auth(_)));
    }

    #[tokio::test]
    async fn valid_cached_token_is_used() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = Cache::new(tmp.path().join("credentials.json"));
        let cfg = config();
        cache
            .put(
                &cfg.api_endpoint,
                CachedToken {
                    id_token: "cached-token".to_string(),
                    refresh_token: None,
                    expires_at: Some(time::OffsetDateTime::now_utc().unix_timestamp() + 3600),
                    issuer: oidc::DEFAULT_ISSUER.to_string(),
                },
            )
            .unwrap();
        let sources = TokenSources::from_config(&cfg, None);
        let auth = Authenticator::new(&cfg, sources).unwrap().with_cache(cache);
        assert_eq!(auth.load_id_token().await.unwrap(), "cached-token");
    }

    // --- Mocked OIDC flows -------------------------------------------------

    use base64::Engine as _;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Build a JWT carrying the given email claim (dummy header/signature).
    fn make_id_token(email: &str) -> String {
        let b64 = |s: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        let payload = format!(r#"{{"sub":"auth0|x","email":"{email}","exp":9999999999}}"#);
        format!("{}.{}.{}", b64(b"{}"), b64(payload.as_bytes()), b64(b"sig"))
    }

    /// Mount the OIDC discovery document pointing at the mock server.
    async fn mount_discovery(server: &MockServer, with_device: bool) {
        let base = server.uri();
        let mut doc = serde_json::json!({ "token_endpoint": format!("{base}/token") });
        if with_device {
            doc["device_authorization_endpoint"] = format!("{base}/device").into();
        }
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(doc))
            .mount(server)
            .await;
    }

    fn test_settings(server: &MockServer) -> OidcSettings {
        OidcSettings {
            issuer: server.uri(),
            client_id: "test-client".to_string(),
            client_secret: None,
            audience: None,
            scope: None,
        }
    }

    #[tokio::test]
    async fn device_code_flow_polls_until_success() {
        let server = MockServer::start().await;
        mount_discovery(&server, true).await;
        Mock::given(method("POST"))
            .and(path("/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "dev-123",
                "user_code": "WXYZ",
                "verification_uri": "https://verify.test",
                "interval": 1,
                "expires_in": 60
            })))
            .mount(&server)
            .await;
        // First poll: pending. Mounted with a usage cap so it is exhausted
        // before the success mock takes over.
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({"error": "authorization_pending"})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id_token": make_id_token("dev@tenzir.test"),
                "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = config();
        cfg.api_endpoint = server.uri();
        let auth = Authenticator::new(&cfg, TokenSources::default())
            .unwrap()
            .with_settings(test_settings(&server))
            .with_cache(Cache::new(tmp.path().join("credentials.json")));

        let claims = auth
            .login(LoginMode::Interactive, |_, _, _| {})
            .await
            .unwrap();
        assert_eq!(claims.display_name(), Some("dev@tenzir.test"));
        // The token was cached for reuse.
        let cached = auth.load_id_token().await.unwrap();
        assert!(!cached.is_empty());
    }

    #[tokio::test]
    async fn client_credentials_flow_mints_token() {
        let server = MockServer::start().await;
        mount_discovery(&server, false).await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=client_credentials"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id_token": make_id_token("ci@tenzir.test"),
                "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = config();
        cfg.api_endpoint = server.uri();
        let mut settings = test_settings(&server);
        settings.client_secret = Some("s3cret".to_string());
        let auth = Authenticator::new(&cfg, TokenSources::default())
            .unwrap()
            .with_settings(settings)
            .with_cache(Cache::new(tmp.path().join("credentials.json")));

        let claims = auth
            .login(LoginMode::NonInteractive, |_, _, _| {})
            .await
            .unwrap();
        assert_eq!(claims.display_name(), Some("ci@tenzir.test"));
    }
}
