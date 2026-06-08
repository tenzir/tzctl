//! The low-level HTTP transport for the three platform target APIs.

use std::collections::BTreeMap;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, Result};

/// The three platform target APIs, each with a distinct path and auth header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetApi {
    /// `{base}/user/<suffix>`, authorized with `X-Tenzir-UserKey`.
    User,
    /// `{base}/user/<suffix>`, no auth header (`id_token` travels in the body).
    UserPublic,
    /// `{base}/admin/<suffix>`, authorized with `X-Tenzir-AdminKey`.
    #[allow(dead_code)] // modeled for completeness; unused in the MVP.
    Admin,
}

impl TargetApi {
    /// The path segment under the base URL for this API.
    fn segment(self) -> &'static str {
        match self {
            TargetApi::User | TargetApi::UserPublic => "user",
            TargetApi::Admin => "admin",
        }
    }
}

/// The auth credential to attach to a request.
enum Auth<'a> {
    /// No auth header (`USER_PUBLIC`).
    None,
    /// A workspace `user_key` for the `USER` API.
    UserKey(&'a str),
}

/// The low-level platform HTTP client.
#[derive(Debug, Clone)]
pub struct AppClient {
    http: reqwest::Client,
    base: String,
    extra_headers: HeaderMap,
}

impl AppClient {
    /// Build a client for `base`, parsing `extra_headers` from the
    /// `TENZIR_PLATFORM_CLI_EXTRA_HEADERS` env var (a JSON object).
    pub fn new(base: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| Error::Platform(format!("cannot build HTTP client: {e}")))?;
        let extra_headers = extra_headers_from_env()?;
        Ok(Self {
            http,
            base: base.into().trim_end_matches('/').to_string(),
            extra_headers,
        })
    }

    /// Build the full URL for a target API and suffix.
    fn url(&self, target: TargetApi, suffix: &str) -> String {
        format!(
            "{}/{}/{}",
            self.base,
            target.segment(),
            suffix.trim_start_matches('/')
        )
    }

    /// POST to a `USER_PUBLIC` endpoint and deserialize the response.
    pub async fn request_public<T: DeserializeOwned>(
        &self,
        suffix: &str,
        body: &impl Serialize,
    ) -> Result<T> {
        let text = self
            .send(TargetApi::UserPublic, suffix, body, Auth::None)
            .await?;
        parse_response(&text, suffix)
    }

    /// POST to a `USER` endpoint with a `user_key` and deserialize.
    pub async fn request_user<T: DeserializeOwned>(
        &self,
        user_key: &str,
        suffix: &str,
        body: &impl Serialize,
    ) -> Result<T> {
        let text = self
            .send(TargetApi::User, suffix, body, Auth::UserKey(user_key))
            .await?;
        parse_response(&text, suffix)
    }

    /// POST to a `USER` endpoint with a `user_key`, returning raw response text.
    #[allow(dead_code)] // used by the node-proxy path (stage 4+).
    pub async fn request_user_raw(
        &self,
        user_key: &str,
        suffix: &str,
        body: &impl Serialize,
    ) -> Result<String> {
        self.send(TargetApi::User, suffix, body, Auth::UserKey(user_key))
            .await
    }

    /// POST a JSON body and return the raw response text, mapping HTTP errors.
    async fn send(
        &self,
        target: TargetApi,
        suffix: &str,
        body: &impl Serialize,
        auth: Auth<'_>,
    ) -> Result<String> {
        let url = self.url(target, suffix);
        let mut req = self.http.post(&url).json(body);
        req = req.headers(self.extra_headers.clone());
        if let Auth::UserKey(key) = auth {
            req = req.header("X-Tenzir-UserKey", key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Platform(format!("request to {url} failed: {e}")))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if status.is_success() {
            return Ok(text);
        }
        Err(map_status_error(status, suffix, &text))
    }
}

/// Deserialize a response body, tolerating an empty body as `null`.
fn parse_response<T: DeserializeOwned>(text: &str, suffix: &str) -> Result<T> {
    let source = if text.trim().is_empty() { "null" } else { text };
    serde_json::from_str(source)
        .map_err(|e| Error::Platform(format!("cannot parse response from {suffix}: {e}")))
}

/// Map a non-success HTTP status to a typed error.
fn map_status_error(status: reqwest::StatusCode, suffix: &str, body: &str) -> Error {
    let detail = summarize_body(body);
    match status.as_u16() {
        410 => Error::NodeDisconnected,
        401 | 403 => Error::Auth(format!(
            "platform returned HTTP {status} for {suffix}{detail}"
        )),
        _ => Error::Platform(format!(
            "platform returned HTTP {status} for {suffix}{detail}"
        )),
    }
}

/// Produce a compact `: <message>` suffix from a (possibly JSON) error body.
fn summarize_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // Prefer a top-level `message`/`error`/`detail` field if present.
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        for key in ["message", "error", "detail"] {
            if let Some(s) = value.get(key).and_then(|v| v.as_str()) {
                return format!(": {s}");
            }
        }
    }
    let oneline: String = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    let clipped = if oneline.len() > 200 {
        format!("{}…", &oneline[..200])
    } else {
        oneline
    };
    format!(": {clipped}")
}

/// Parse the `TENZIR_PLATFORM_CLI_EXTRA_HEADERS` env var into a header map.
fn extra_headers_from_env() -> Result<HeaderMap> {
    let raw = match std::env::var("TENZIR_PLATFORM_CLI_EXTRA_HEADERS") {
        Ok(v) if !v.is_empty() => v,
        _ => return Ok(HeaderMap::new()),
    };
    let map: BTreeMap<String, String> = serde_json::from_str(&raw).map_err(|e| {
        Error::Config(format!(
            "TENZIR_PLATFORM_CLI_EXTRA_HEADERS must be a JSON object of strings: {e}"
        ))
    })?;
    let mut headers = HeaderMap::new();
    for (k, v) in map {
        let name = HeaderName::from_bytes(k.as_bytes())
            .map_err(|e| Error::Config(format!("invalid extra header name {k:?}: {e}")))?;
        let value = HeaderValue::from_str(&v)
            .map_err(|e| Error::Config(format!("invalid extra header value for {k:?}: {e}")))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_construction() {
        let client = AppClient::new("https://api.test/base/").unwrap();
        assert_eq!(
            client.url(TargetApi::User, "switch-tenant"),
            "https://api.test/base/user/switch-tenant"
        );
        assert_eq!(
            client.url(TargetApi::Admin, "/foo"),
            "https://api.test/base/admin/foo"
        );
    }

    #[test]
    fn maps_410_to_disconnected() {
        let err = map_status_error(reqwest::StatusCode::GONE, "pipeline/list", "");
        assert!(matches!(err, Error::NodeDisconnected));
    }

    #[test]
    fn maps_401_to_auth() {
        let err = map_status_error(
            reqwest::StatusCode::UNAUTHORIZED,
            "x",
            "{\"message\":\"nope\"}",
        );
        match err {
            Error::Auth(msg) => assert!(msg.contains("nope")),
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[test]
    fn maps_other_to_platform() {
        let err = map_status_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "x", "boom");
        match err {
            Error::Platform(msg) => assert!(msg.contains("boom")),
            other => panic!("expected Platform, got {other:?}"),
        }
    }
}
