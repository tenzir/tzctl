//! Wire types for running short-lived, hidden TQL queries on a node.
//!
//! `tz pipeline status` inspects a pipeline by launching a bounded, hidden
//! pipeline through the node-proxy `pipeline/launch` endpoint and draining its
//! results through `serve`. This mirrors how the platform web app composes
//! diagnostics and performance views (there is no dedicated status endpoint).

use serde::{Deserialize, Serialize};

/// The `autostart` block of a `pipeline/launch` request.
///
/// Setting `created = true` starts the pipeline as soon as it is created, so we
/// never need a follow-up `pipeline/update { action: "start" }`.
#[derive(Debug, Serialize)]
pub struct Autostart {
    /// Start the pipeline immediately upon creation.
    pub created: bool,
}

/// The `pipeline/launch` request body for a hidden, short-lived query.
#[derive(Debug, Serialize)]
pub struct LaunchRequest<'a> {
    /// The client-generated pipeline id.
    pub id: &'a str,
    /// The TQL definition to run.
    pub definition: &'a str,
    /// The id under which results are served (usually equal to `id`).
    pub serve_id: &'a str,
    /// Hide the pipeline from the UI and `pipeline/list` reconciliation.
    pub hidden: bool,
    /// A short time-to-live; refreshed via keepalive for long queries (unused
    /// here, as all status queries complete well within the TTL).
    pub ttl: &'a str,
    /// Start the pipeline as soon as it is created.
    pub autostart: Autostart,
}

impl<'a> LaunchRequest<'a> {
    /// Build a launch request for a bounded, hidden query with a 60 s TTL.
    pub fn hidden_query(id: &'a str, serve_id: &'a str, definition: &'a str) -> Self {
        Self {
            id,
            definition,
            serve_id,
            hidden: true,
            ttl: "60s",
            autostart: Autostart { created: true },
        }
    }
}

/// The `pipeline/launch` response body (defensively typed).
///
/// We only need to know whether the launch succeeded; on failure the node
/// reports diagnostics we can surface. Extra fields are ignored.
#[derive(Debug, Default, Deserialize)]
pub struct LaunchResponse {
    /// The launched pipeline id, when the definition is deployable.
    #[serde(default)]
    #[allow(dead_code)] // parsed for completeness; the client uses its own id.
    pub id: Option<String>,
}

/// The `serve` request body for draining a served pipeline.
#[derive(Debug, Serialize)]
pub struct ServeRequest<'a> {
    /// The `serve_id` the pipeline was launched with.
    pub serve_id: &'a str,
    /// The maximum number of events to return in this page (node caps at 1024).
    pub max_events: usize,
    /// The continuation token from the previous page, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation_token: Option<&'a str>,
    /// Whether to include schema information (`"never"` for status queries).
    pub schema: &'static str,
}

impl<'a> ServeRequest<'a> {
    /// A serve request for one page of up to `max_events` events.
    pub fn page(serve_id: &'a str, max_events: usize, continuation_token: Option<&'a str>) -> Self {
        Self {
            serve_id,
            max_events: max_events.min(1024),
            continuation_token,
            schema: "never",
        }
    }
}

/// One page of a `serve` response.
#[derive(Debug, Default, Deserialize)]
pub struct ServeResponse {
    /// The events in this page (opaque JSON, shaped by the query).
    #[serde(default)]
    pub events: Vec<serde_json::Value>,
    /// The token to fetch the next page; absent when the stream is exhausted.
    #[serde(default)]
    pub next_continuation_token: Option<String>,
    /// The pipeline run-state; `"failed"` on the final page means the query
    /// pipeline itself failed.
    #[serde(default)]
    pub state: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_body_is_hidden_with_ttl_and_autostart() {
        let body =
            serde_json::to_value(LaunchRequest::hidden_query("pid", "pid", "diagnostics")).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "id": "pid",
                "definition": "diagnostics",
                "serve_id": "pid",
                "hidden": true,
                "ttl": "60s",
                "autostart": {"created": true}
            })
        );
    }

    #[test]
    fn serve_body_caps_max_events_and_omits_absent_token() {
        let body = serde_json::to_value(ServeRequest::page("sid", 5000, None)).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"serve_id": "sid", "max_events": 1024, "schema": "never"})
        );
    }

    #[test]
    fn serve_body_includes_token_when_present() {
        let body = serde_json::to_value(ServeRequest::page("sid", 10, Some("tok"))).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "serve_id": "sid",
                "max_events": 10,
                "continuation_token": "tok",
                "schema": "never"
            })
        );
    }

    #[test]
    fn serve_response_parses_page() {
        let raw = r#"{"events":[{"a":1}],"next_continuation_token":"tok","state":"running"}"#;
        let resp: ServeResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.events.len(), 1);
        assert_eq!(resp.next_continuation_token.as_deref(), Some("tok"));
        assert_eq!(resp.state.as_deref(), Some("running"));
    }

    #[test]
    fn serve_response_tolerates_empty_body() {
        let resp: ServeResponse = serde_json::from_str("{}").unwrap();
        assert!(resp.events.is_empty());
        assert!(resp.next_continuation_token.is_none());
    }
}
