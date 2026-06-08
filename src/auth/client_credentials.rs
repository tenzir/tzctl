//! The OAuth 2.0 client-credentials grant (non-interactive login).

use super::oidc::{DEFAULT_CLIENT_CREDENTIALS_SCOPE, DiscoveryDocument, OidcSettings};
use super::token::TokenResponse;
use crate::error::{Error, Result};

/// Mint a token via the client-credentials grant.
///
/// Requires `settings.client_secret` to be set; applies `audience`/`scope`
/// overrides, defaulting the scope to `openid`.
pub async fn run(
    http: &reqwest::Client,
    settings: &OidcSettings,
    discovery: &DiscoveryDocument,
) -> Result<TokenResponse> {
    let secret = settings.client_secret.as_deref().ok_or_else(|| {
        Error::Auth("client-credentials flow requires a client secret".to_string())
    })?;
    let scope = settings
        .scope
        .as_deref()
        .unwrap_or(DEFAULT_CLIENT_CREDENTIALS_SCOPE);

    let form = vec![
        ("grant_type", "client_credentials"),
        ("client_id", settings.client_id.as_str()),
        ("client_secret", secret),
        ("scope", scope),
        ("audience", settings.effective_audience()),
    ];

    // The platform CLI also authenticates the client via HTTP Basic auth
    // (`client_secret_basic`), in addition to the form body.
    let resp = http
        .post(&discovery.token_endpoint)
        .basic_auth(&settings.client_id, Some(secret))
        .form(&form)
        .send()
        .await
        .map_err(|e| Error::Auth(format!("client-credentials request failed: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(Error::Auth(format!(
            "client-credentials grant returned HTTP {status}: {body}"
        )));
    }
    resp.json::<TokenResponse>()
        .await
        .map_err(|e| Error::Auth(format!("cannot parse token response: {e}")))
}
