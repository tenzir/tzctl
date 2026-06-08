//! The OAuth 2.0 device authorization grant (interactive login).

use std::time::Duration;

use serde::Deserialize;

use super::oidc::{DEFAULT_DEVICE_SCOPE, DiscoveryDocument, OidcSettings};
use super::token::TokenResponse;
use crate::error::{Error, Result};

/// The device-authorization endpoint response.
#[derive(Debug, Clone, Deserialize)]
struct DeviceAuthResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default = "default_interval")]
    interval: u64,
    expires_in: u64,
}

/// The OAuth default polling interval when the server omits one.
fn default_interval() -> u64 {
    5
}

/// An OAuth token-endpoint error body (used during polling).
#[derive(Debug, Deserialize)]
struct TokenError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// Run the device-code flow, returning the token response on success.
///
/// The `notify` closure receives a user-facing prompt (verification URL + code)
/// so the caller controls presentation. Polling honors the server `interval`
/// and `slow_down` responses and respects the device-code expiry.
pub async fn run(
    http: &reqwest::Client,
    settings: &OidcSettings,
    discovery: &DiscoveryDocument,
    notify: impl FnOnce(&str, &str, Option<&str>),
) -> Result<TokenResponse> {
    let device_endpoint = discovery
        .device_authorization_endpoint
        .as_deref()
        .ok_or_else(|| {
            Error::Auth(
                "the OIDC provider does not advertise a device authorization endpoint".to_string(),
            )
        })?;
    let scope = settings.scope.as_deref().unwrap_or(DEFAULT_DEVICE_SCOPE);

    // Only forward an explicitly-configured audience. Unlike the
    // client-credentials flow, the device flow must not fall back to the
    // client id: Auth0 rejects an unregistered API audience with
    // "Service not enabled within domain".
    let mut form = vec![("client_id", settings.client_id.as_str()), ("scope", scope)];
    if let Some(aud) = &settings.audience {
        form.push(("audience", aud.as_str()));
    }
    let auth: DeviceAuthResponse = http
        .post(device_endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|e| Error::Auth(format!("device authorization request failed: {e}")))?
        .error_for_status()
        .map_err(|e| Error::Auth(format!("device authorization rejected: {e}")))?
        .json()
        .await
        .map_err(|e| Error::Auth(format!("cannot parse device authorization response: {e}")))?;

    notify(
        &auth.verification_uri,
        &auth.user_code,
        auth.verification_uri_complete.as_deref(),
    );

    poll_for_token(http, settings, &discovery.token_endpoint, &auth).await
}

/// Poll the token endpoint until the user approves, the grant expires, or an
/// unrecoverable error occurs.
async fn poll_for_token(
    http: &reqwest::Client,
    settings: &OidcSettings,
    token_endpoint: &str,
    auth: &DeviceAuthResponse,
) -> Result<TokenResponse> {
    let mut interval = auth.interval.max(1);
    let deadline = time::OffsetDateTime::now_utc() + Duration::from_secs(auth.expires_in.max(1));

    loop {
        if time::OffsetDateTime::now_utc() >= deadline {
            return Err(Error::Auth(
                "device login timed out before approval".to_string(),
            ));
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;

        let form = [
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", auth.device_code.as_str()),
            ("client_id", settings.client_id.as_str()),
        ];
        let resp = http
            .post(token_endpoint)
            .form(&form)
            .send()
            .await
            .map_err(|e| Error::Auth(format!("token polling request failed: {e}")))?;

        if resp.status().is_success() {
            return resp
                .json::<TokenResponse>()
                .await
                .map_err(|e| Error::Auth(format!("cannot parse token response: {e}")));
        }

        let err: TokenError = resp
            .json()
            .await
            .map_err(|e| Error::Auth(format!("cannot parse token error response: {e}")))?;
        match err.error.as_str() {
            "authorization_pending" => continue,
            "slow_down" => {
                interval += 5;
                continue;
            }
            "expired_token" => {
                return Err(Error::Auth("device login expired".to_string()));
            }
            "access_denied" => {
                return Err(Error::Auth("device login was denied".to_string()));
            }
            other => {
                let detail = err
                    .error_description
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default();
                return Err(Error::Auth(format!(
                    "device login failed ({other}){detail}"
                )));
            }
        }
    }
}
