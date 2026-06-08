//! Token response parsing and lightweight (unverified) JWT claim decoding.
//!
//! No signature verification is performed client-side; the platform validates
//! tokens. Claims are decoded only for display (e.g. the logged-in user).

use base64::Engine as _;
use serde::Deserialize;

use crate::error::{Error, Result};

/// A successful OAuth token endpoint response.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    /// The OIDC ID token (what the platform consumes).
    pub id_token: Option<String>,
    /// The access token (unused by `tz` but parsed for completeness).
    #[serde(default)]
    #[allow(dead_code)]
    pub access_token: Option<String>,
    /// A refresh token, when the provider issues one.
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Lifetime of the token in seconds.
    #[serde(default)]
    pub expires_in: Option<i64>,
}

/// Non-sensitive claims decoded from an ID token's payload, for display only.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Claims {
    /// The subject (user) identifier.
    #[serde(default)]
    pub sub: Option<String>,
    /// The user's email, if present.
    #[serde(default)]
    pub email: Option<String>,
    /// The user's display name, if present.
    #[serde(default)]
    pub name: Option<String>,
    /// Expiry as a Unix timestamp (seconds).
    #[serde(default)]
    pub exp: Option<i64>,
}

impl Claims {
    /// A best-effort human label for the authenticated user.
    pub fn display_name(&self) -> Option<&str> {
        self.email
            .as_deref()
            .or(self.name.as_deref())
            .or(self.sub.as_deref())
    }
}

/// Decode the (unverified) payload claims of a JWT.
pub fn decode_claims(jwt: &str) -> Result<Claims> {
    let payload = jwt
        .split('.')
        .nth(1)
        .ok_or_else(|| Error::Auth("malformed id_token: not a JWT".to_string()))?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|e| Error::Auth(format!("cannot base64-decode id_token payload: {e}")))?;
    serde_json::from_slice::<Claims>(&bytes)
        .map_err(|e| Error::Auth(format!("cannot parse id_token claims: {e}")))
}

/// Whether the token is expired (or expires within `skew` seconds).
// Consumed by the token-resolution path (and the client stage).
#[allow(dead_code)]
pub fn is_expired(exp: i64, skew_seconds: i64) -> bool {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    now + skew_seconds >= exp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a JWT with the given JSON payload (header/signature are dummies).
    fn make_jwt(payload_json: &str) -> String {
        let b64 = |s: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        format!(
            "{}.{}.{}",
            b64(b"{\"alg\":\"none\"}"),
            b64(payload_json.as_bytes()),
            b64(b"sig")
        )
    }

    #[test]
    fn decodes_claims() {
        let jwt = make_jwt(r#"{"sub":"auth0|abc","email":"a@b.test","exp":123}"#);
        let claims = decode_claims(&jwt).unwrap();
        assert_eq!(claims.sub.as_deref(), Some("auth0|abc"));
        assert_eq!(claims.display_name(), Some("a@b.test"));
        assert_eq!(claims.exp, Some(123));
    }

    #[test]
    fn rejects_non_jwt() {
        assert!(decode_claims("notajwt").is_err());
    }

    #[test]
    fn expiry_detection() {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        assert!(is_expired(now - 10, 0));
        assert!(!is_expired(now + 3600, 60));
        // Within skew window counts as expired.
        assert!(is_expired(now + 30, 60));
    }
}
