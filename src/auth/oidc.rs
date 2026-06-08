//! OIDC settings and discovery-document handling.

use serde::Deserialize;

use crate::config::ResolvedConfig;
use crate::error::{Error, Result};

/// The default public Auth0 issuer for the Tenzir Platform.
pub const DEFAULT_ISSUER: &str = "https://tenzir.eu.auth0.com/";

/// The default public OIDC client id.
pub const DEFAULT_CLIENT_ID: &str = "vzRh8grIVu1bwutvZbbpBDCOvSzN8AXh";

/// The default scope for the interactive device-code flow.
pub const DEFAULT_DEVICE_SCOPE: &str = "openid email";

/// The default scope for the non-interactive client-credentials flow.
pub const DEFAULT_CLIENT_CREDENTIALS_SCOPE: &str = "openid";

/// Resolved OIDC parameters, derived from the `[platform.oidc]` config table
/// and built-in defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcSettings {
    /// The OIDC issuer URL (no trailing-slash assumptions are made).
    pub issuer: String,
    /// The OAuth client id.
    pub client_id: String,
    /// The OAuth client secret, when using client-credentials.
    pub client_secret: Option<String>,
    /// An optional audience override.
    pub audience: Option<String>,
    /// An optional scope override (otherwise flow-specific defaults apply).
    pub scope: Option<String>,
}

impl Default for OidcSettings {
    fn default() -> Self {
        Self {
            issuer: DEFAULT_ISSUER.to_string(),
            client_id: DEFAULT_CLIENT_ID.to_string(),
            client_secret: None,
            audience: None,
            scope: None,
        }
    }
}

impl OidcSettings {
    /// Resolve settings from the `[platform.oidc]` config table over defaults.
    pub fn from_config(config: &ResolvedConfig) -> Self {
        let mut settings = Self::default();
        if let Some(issuer) = &config.oidc_issuer {
            settings.issuer = issuer.clone();
        }
        if let Some(client_id) = &config.client_id {
            settings.client_id = client_id.clone();
        }
        settings.client_secret = config.client_secret.clone();
        settings.audience = config.oidc_audience.clone();
        settings.scope = config.oidc_scope.clone();
        settings
    }

    /// The effective audience for token requests.
    ///
    /// Falls back to the `client_id` when no explicit audience is configured,
    /// mirroring the Python CLI (`oidc.py:65-66`).
    pub fn effective_audience(&self) -> &str {
        self.audience.as_deref().unwrap_or(&self.client_id)
    }

    /// The URL of the OIDC discovery document for this issuer.
    pub fn discovery_url(&self) -> String {
        let base = self.issuer.trim_end_matches('/');
        format!("{base}/.well-known/openid-configuration")
    }
}

/// The subset of the OIDC discovery document that we consume.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscoveryDocument {
    /// The token endpoint URL.
    pub token_endpoint: String,
    /// The device authorization endpoint URL (absent on some providers).
    #[serde(default)]
    pub device_authorization_endpoint: Option<String>,
}

/// Fetch and parse the OIDC discovery document for `settings`.
pub async fn discover(
    http: &reqwest::Client,
    settings: &OidcSettings,
) -> Result<DiscoveryDocument> {
    let url = settings.discovery_url();
    let resp = http
        .get(&url)
        .send()
        .await
        .map_err(|e| Error::Auth(format!("OIDC discovery request to {url} failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(Error::Auth(format!(
            "OIDC discovery at {url} returned HTTP {}",
            resp.status()
        )));
    }
    resp.json::<DiscoveryDocument>()
        .await
        .map_err(|e| Error::Auth(format!("cannot parse OIDC discovery document: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_url_handles_trailing_slash() {
        let s = OidcSettings {
            issuer: "https://example.test/".to_string(),
            ..Default::default()
        };
        assert_eq!(
            s.discovery_url(),
            "https://example.test/.well-known/openid-configuration"
        );
        let s2 = OidcSettings {
            issuer: "https://example.test".to_string(),
            ..Default::default()
        };
        assert_eq!(
            s2.discovery_url(),
            "https://example.test/.well-known/openid-configuration"
        );
    }

    #[test]
    fn defaults_are_public_platform() {
        let s = OidcSettings::default();
        assert_eq!(s.issuer, DEFAULT_ISSUER);
        assert_eq!(s.client_id, DEFAULT_CLIENT_ID);
        assert!(s.client_secret.is_none());
    }
}
