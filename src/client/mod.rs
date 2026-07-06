//! Platform client: the `PlatformClient` trait, the node-proxy transport, and
//! error mapping.

pub mod pipelines;
pub mod query;
pub mod session;
pub mod transport;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::config::ResolvedConfig;
use crate::error::{Error, Result};
use crate::model::{
    DesiredPipeline, Node, NodeId, PipelineId, RemotePipeline, TenantId, TransitionAction,
    Workspace,
};
use pipelines::{
    CreateRequest, CreateResponse, DeleteRequest, ListRequest, ListResponse, UpdateRequest,
};
use query::{LaunchRequest, LaunchResponse, ResetTtlRequest, ServeRequest, ServeResponse};
use session::Session;

/// The high-level platform interface the CLI and reconciler depend on.
///
/// Pipeline-specific methods are added in later stages; the node-proxy escape
/// hatch already lets callers reach any node endpoint.
// Returned futures are not `Send`-bounded: `tz` drives them on a current-thread
// flow, so the default `async fn` desugaring is sufficient.
#[allow(async_fn_in_trait)]
pub trait PlatformClient {
    /// List the workspaces the authenticated user can access.
    async fn list_workspaces(&self) -> Result<Vec<Workspace>>;

    /// List the nodes in a workspace.
    async fn list_nodes(&self, workspace: &TenantId) -> Result<Vec<Node>>;

    /// Relay a request to a node endpoint through the platform node-proxy.
    async fn node_proxy<T: DeserializeOwned>(
        &self,
        workspace: &TenantId,
        node: &NodeId,
        endpoint: &str,
        body: &impl Serialize,
    ) -> Result<T>;

    /// List the pipelines on a node via the node-proxy `pipeline/list`.
    ///
    /// Provided in terms of [`Self::node_proxy`]; implementors rarely override.
    async fn list_pipelines(
        &self,
        workspace: &TenantId,
        node: &NodeId,
    ) -> Result<Vec<RemotePipeline>> {
        let resp: ListResponse = self
            .node_proxy(workspace, node, "pipeline/list", &ListRequest {})
            .await?;
        Ok(resp.pipelines)
    }

    /// Create a pipeline and drive it to the desired state; return its id.
    ///
    /// A freshly-created pipeline is in `Created`; this issues the transitions
    /// from [`DesiredPipeline::create_transitions`] so the pipeline ends in the
    /// requested state (default running).
    async fn create(
        &self,
        workspace: &TenantId,
        node: &NodeId,
        pipeline: &DesiredPipeline,
    ) -> Result<PipelineId> {
        let resp: CreateResponse = self
            .node_proxy(
                workspace,
                node,
                "pipeline/create",
                &CreateRequest::from_desired(pipeline),
            )
            .await?;
        for action in pipeline.create_transitions() {
            self.transition(workspace, node, &resp.id, action).await?;
        }
        Ok(resp.id)
    }

    /// Update a pipeline's `definition`/`name` in place via `pipeline/update`.
    async fn set(
        &self,
        workspace: &TenantId,
        node: &NodeId,
        id: &PipelineId,
        pipeline: &DesiredPipeline,
    ) -> Result<()> {
        let _: serde_json::Value = self
            .node_proxy(
                workspace,
                node,
                "pipeline/update",
                &UpdateRequest::edit(id.as_str(), pipeline),
            )
            .await?;
        Ok(())
    }

    /// Delete a pipeline by id via `pipeline/delete`.
    async fn delete(&self, workspace: &TenantId, node: &NodeId, id: &PipelineId) -> Result<()> {
        let _: serde_json::Value = self
            .node_proxy(
                workspace,
                node,
                "pipeline/delete",
                &DeleteRequest { id: id.as_str() },
            )
            .await?;
        Ok(())
    }

    /// Apply a run-state transition via `pipeline/update`.
    async fn transition(
        &self,
        workspace: &TenantId,
        node: &NodeId,
        id: &PipelineId,
        action: TransitionAction,
    ) -> Result<()> {
        let _: serde_json::Value = self
            .node_proxy(
                workspace,
                node,
                "pipeline/update",
                &UpdateRequest::action(id.as_str(), action),
            )
            .await?;
        Ok(())
    }

    /// Resolve a pipeline by exact name, erroring on not-found or ambiguity.
    async fn resolve_pipeline(
        &self,
        workspace: &TenantId,
        node: &NodeId,
        name: &str,
    ) -> Result<RemotePipeline> {
        let pipelines = self.list_pipelines(workspace, node).await?;
        let mut matches = pipelines.into_iter().filter(|p| p.name == name);
        let first = matches
            .next()
            .ok_or_else(|| Error::Platform(format!("no pipeline named {name:?} on the node")))?;
        if matches.next().is_some() {
            return Err(Error::Platform(format!(
                "multiple pipelines named {name:?}; names must be unique"
            )));
        }
        Ok(first)
    }

    /// Run a bounded, hidden TQL query on a node and collect its events.
    ///
    /// Launches a hidden pipeline (60 s TTL, autostart) via `pipeline/launch`,
    /// drains its results through `serve` (following continuation tokens up to
    /// `max_events`), then makes a best-effort attempt to stop it early. This
    /// is the mechanism behind `tz pipeline status`.
    async fn run_query(
        &self,
        workspace: &TenantId,
        node: &NodeId,
        definition: &str,
        max_events: usize,
    ) -> Result<Vec<serde_json::Value>> {
        let id = uuid::Uuid::new_v4().to_string();
        let launch = LaunchRequest::hidden_query(&id, &id, definition);
        let _: LaunchResponse = self
            .node_proxy(workspace, node, "pipeline/launch", &launch)
            .await?;

        let mut events = Vec::new();
        let mut token: Option<String> = None;
        let result = loop {
            let remaining = max_events.saturating_sub(events.len());
            if remaining == 0 {
                break Ok(());
            }
            let req = ServeRequest::page(&id, remaining, token.as_deref());
            let page: ServeResponse = match self.node_proxy(workspace, node, "serve", &req).await {
                Ok(page) => page,
                Err(e) => break Err(e),
            };
            events.extend(page.events);
            match page.next_continuation_token {
                Some(next) => token = Some(next),
                None => {
                    if page.state.as_deref() == Some("failed") {
                        break Err(Error::Platform(
                            "the status query pipeline failed on the node".to_string(),
                        ));
                    }
                    break Ok(());
                }
            }
        };

        // Best-effort cleanup; the TTL also reaps the pipeline.
        let _ = self
            .transition(
                workspace,
                node,
                &PipelineId(id),
                crate::model::TransitionAction::Stop,
            )
            .await;

        result.map(|()| events)
    }

    /// Sample the output of a live TQL query for a fixed duration.
    ///
    /// Launches a hidden pipeline and drains its `serve` output until `window`
    /// elapses (or the pipeline ends, or Ctrl-C), then stops it. Unlike
    /// [`Self::run_query`], the bound is wall-clock time rather than an event
    /// count, so the number of collected events scales with the query's output
    /// rate — essential for `metrics ..., live=true` where each tick emits one
    /// row per operator and a row cap would truncate mid-tick. Backs
    /// `tz pipeline insights`.
    async fn sample_live_query(
        &self,
        workspace: &TenantId,
        node: &NodeId,
        definition: &str,
        window: std::time::Duration,
    ) -> Result<Vec<serde_json::Value>> {
        let id = uuid::Uuid::new_v4().to_string();
        let launch = LaunchRequest::hidden_query(&id, &id, definition);
        let _: LaunchResponse = self
            .node_proxy(workspace, node, "pipeline/launch", &launch)
            .await?;

        // Stop draining once the sampling window elapses.
        let cancel = tokio::sync::Notify::new();
        let timer = async {
            tokio::time::sleep(window).await;
            cancel.notify_one();
        };

        let mut events = Vec::new();
        let mut collect = |page: &[serde_json::Value]| -> Result<()> {
            events.extend_from_slice(page);
            Ok(())
        };
        let result = tokio::join!(
            self.stream_loop(workspace, node, &id, &mut collect, Some(&cancel)),
            timer,
        )
        .0;

        // Best-effort cleanup; the TTL also reaps the pipeline.
        let _ = self
            .transition(
                workspace,
                node,
                &PipelineId(id),
                crate::model::TransitionAction::Stop,
            )
            .await;

        result.map(|()| events)
    }

    /// Refresh a served pipeline's TTL via `pipeline/reset-ttl`.
    async fn reset_ttl(&self, workspace: &TenantId, node: &NodeId, id: &str) -> Result<()> {
        let _: serde_json::Value = self
            .node_proxy(
                workspace,
                node,
                "pipeline/reset-ttl",
                &ResetTtlRequest::one(id),
            )
            .await?;
        Ok(())
    }

    /// Launch a hidden pipeline and stream its served results incrementally.
    ///
    /// Each page of events is handed to `on_events` as it arrives; the loop
    /// follows continuation tokens until the pipeline terminates (no token),
    /// refreshing the TTL every 30 s so long-running pipelines stay alive. The
    /// stream is interrupted cleanly on Ctrl-C, and the pipeline is stopped on
    /// exit.
    #[allow(dead_code)] // single-stream building block; exercised by tests.
    async fn stream_query<F>(
        &self,
        workspace: &TenantId,
        node: &NodeId,
        definition: &str,
        mut on_events: F,
    ) -> Result<()>
    where
        F: FnMut(&[serde_json::Value]) -> Result<()>,
    {
        let id = uuid::Uuid::new_v4().to_string();
        let launch = LaunchRequest::hidden_query(&id, &id, definition);
        let _: LaunchResponse = self
            .node_proxy(workspace, node, "pipeline/launch", &launch)
            .await?;

        let result = self
            .stream_loop(workspace, node, &id, &mut on_events, None)
            .await;

        // Best-effort cleanup; the TTL also reaps the pipeline.
        let _ = self
            .transition(
                workspace,
                node,
                &PipelineId(id),
                crate::model::TransitionAction::Stop,
            )
            .await;

        result
    }

    /// Run a pipeline while concurrently streaming its runtime diagnostics.
    ///
    /// Launches the given `definition` and, alongside it, a companion hidden
    /// pipeline that tails `diagnostics live=true, retro=true` filtered to the
    /// main pipeline's id. Result events go to `on_events`; diagnostics go to
    /// `on_diagnostic`, both as they arrive. When the main pipeline terminates
    /// (or on Ctrl-C) the diagnostics loop is cancelled and both pipelines are
    /// stopped. This backs `tz run`.
    async fn stream_run<E, D>(
        &self,
        workspace: &TenantId,
        node: &NodeId,
        definition: &str,
        mut on_events: E,
        mut on_diagnostic: D,
    ) -> Result<()>
    where
        E: FnMut(&[serde_json::Value]) -> Result<()>,
        D: FnMut(&[serde_json::Value]) -> Result<()>,
    {
        let main_id = uuid::Uuid::new_v4().to_string();
        let launch = LaunchRequest::hidden_query(&main_id, &main_id, definition);
        let _: LaunchResponse = self
            .node_proxy(workspace, node, "pipeline/launch", &launch)
            .await?;

        let diag_id = uuid::Uuid::new_v4().to_string();
        let diag_def = format!(
            "remote {{ diagnostics live=true, retro=true | where pipeline_id == \"{main_id}\" }}"
        );
        let diag_launch = LaunchRequest::hidden_query(&diag_id, &diag_id, &diag_def);
        let _: LaunchResponse = self
            .node_proxy(workspace, node, "pipeline/launch", &diag_launch)
            .await?;

        // Cancels the (otherwise endless) live-diagnostics loop once the main
        // pipeline completes. `notify_one` stores a permit, so there is no race
        // if the main loop finishes before the diagnostics loop starts waiting.
        let cancel = tokio::sync::Notify::new();

        let main_fut = async {
            let r = self
                .stream_loop(workspace, node, &main_id, &mut on_events, None)
                .await;
            cancel.notify_one();
            r
        };
        let diag_fut =
            self.stream_loop(workspace, node, &diag_id, &mut on_diagnostic, Some(&cancel));

        let (main_res, _diag_res) = tokio::join!(main_fut, diag_fut);

        // Best-effort cleanup of both pipelines; their TTLs also reap them.
        let _ = self
            .transition(
                workspace,
                node,
                &PipelineId(main_id),
                crate::model::TransitionAction::Stop,
            )
            .await;
        let _ = self
            .transition(
                workspace,
                node,
                &PipelineId(diag_id),
                crate::model::TransitionAction::Stop,
            )
            .await;

        main_res
    }

    /// The serve/keepalive loop shared by [`Self::stream_query`] and
    /// [`Self::stream_run`].
    ///
    /// When `cancel` is `Some`, the loop also returns as soon as it is
    /// notified (used to stop the endless live-diagnostics stream).
    async fn stream_loop<F>(
        &self,
        workspace: &TenantId,
        node: &NodeId,
        id: &str,
        on_events: &mut F,
        cancel: Option<&tokio::sync::Notify>,
    ) -> Result<()>
    where
        F: FnMut(&[serde_json::Value]) -> Result<()>,
    {
        // A future that resolves when cancelled, or never when `cancel` is None.
        let cancel_fut = async {
            match cancel {
                Some(notify) => notify.notified().await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(cancel_fut);

        let mut token: Option<String> = None;
        let mut last_ttl = std::time::Instant::now();
        loop {
            let req = ServeRequest::page(id, 1024, token.as_deref());
            let page: ServeResponse = tokio::select! {
                biased;
                _ = &mut cancel_fut => return Ok(()),
                _ = tokio::signal::ctrl_c() => return Ok(()),
                page = self.node_proxy(workspace, node, "serve", &req) => page?,
            };
            if !page.events.is_empty() {
                on_events(&page.events)?;
            }
            if last_ttl.elapsed() >= std::time::Duration::from_secs(30) {
                let _ = self.reset_ttl(workspace, node, id).await;
                last_ttl = std::time::Instant::now();
            }
            match page.next_continuation_token {
                Some(next) => token = Some(next),
                None => {
                    if page.state.as_deref() == Some("failed") {
                        return Err(Error::Platform(
                            "the pipeline failed on the node".to_string(),
                        ));
                    }
                    return Ok(());
                }
            }
        }
    }
}

/// The real, `AppClient`-backed platform client.
pub struct PlatformApi {
    session: Session,
}

impl PlatformApi {
    /// Build a client from resolved config and an `id_token`.
    pub fn new(config: &ResolvedConfig, id_token: impl Into<String>) -> Result<Self> {
        let session = Session::new(config.api_endpoint.clone(), id_token)?;
        Ok(Self { session })
    }

    /// Access the underlying session (key exchange, caching).
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Wrap an existing session (used in tests to inject a temp cache).
    #[cfg(test)]
    pub fn from_session(session: Session) -> Self {
        Self { session }
    }

    /// The node-proxy endpoint suffix for a workspace/node/endpoint triple.
    #[allow(dead_code)] // exercised by tests; used by node_proxy (stage 4+).
    fn proxy_suffix(workspace: &TenantId, node: &NodeId, endpoint: &str) -> String {
        format!(
            "node-proxy/{}/{}/{}",
            workspace.as_str(),
            node.as_str(),
            endpoint.trim_start_matches('/')
        )
    }
}

impl PlatformClient for PlatformApi {
    async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        self.session.login_info().await
    }

    async fn list_nodes(&self, workspace: &TenantId) -> Result<Vec<Node>> {
        self.session.list_nodes(workspace).await
    }

    async fn node_proxy<T: DeserializeOwned>(
        &self,
        workspace: &TenantId,
        node: &NodeId,
        endpoint: &str,
        body: &impl Serialize,
    ) -> Result<T> {
        let suffix = Self::proxy_suffix(workspace, node, endpoint);
        self.session
            .with_key_retry(workspace, |key| {
                let suffix = suffix.clone();
                async move {
                    let text = self
                        .session
                        .client()
                        .request_user_raw(&key, &suffix, body)
                        .await?;
                    let source = if text.trim().is_empty() {
                        "null"
                    } else {
                        &text
                    };
                    serde_json::from_str::<T>(source).map_err(|e| {
                        Error::Platform(format!("cannot parse node-proxy response: {e}"))
                    })
                }
            })
            .await
    }
}

/// An in-memory [`PlatformClient`] for tests and the declarative-core stage.
#[cfg(test)]
#[derive(Default)]
pub struct MockClient {
    /// Workspaces returned by [`PlatformClient::list_workspaces`].
    pub workspaces: Vec<Workspace>,
    /// Nodes returned by [`PlatformClient::list_nodes`], keyed by tenant id.
    pub nodes: std::collections::HashMap<String, Vec<Node>>,
    /// Canned node-proxy responses keyed by endpoint, as raw JSON.
    pub proxy_responses: std::collections::HashMap<String, String>,
    /// Endpoints that should report the node as disconnected.
    pub disconnected_endpoints: std::collections::HashSet<String>,
}

#[cfg(test)]
impl PlatformClient for MockClient {
    async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        Ok(self.workspaces.clone())
    }

    async fn list_nodes(&self, workspace: &TenantId) -> Result<Vec<Node>> {
        Ok(self
            .nodes
            .get(workspace.as_str())
            .cloned()
            .unwrap_or_default())
    }

    async fn node_proxy<T: DeserializeOwned>(
        &self,
        _workspace: &TenantId,
        _node: &NodeId,
        endpoint: &str,
        _body: &impl Serialize,
    ) -> Result<T> {
        if self.disconnected_endpoints.contains(endpoint) {
            return Err(Error::NodeDisconnected);
        }
        let raw = self
            .proxy_responses
            .get(endpoint)
            .map(String::as_str)
            .unwrap_or("null");
        serde_json::from_str::<T>(raw)
            .map_err(|e| Error::Platform(format!("mock parse error: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::cache::{Cache, CachedUserKey};
    use crate::client::session::Session;
    use crate::model::{DesiredPipeline, DesiredState};
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Build a `PlatformApi` against `server` with a pre-seeded workspace key.
    fn api_for(server: &MockServer, tmp: &tempfile::TempDir) -> PlatformApi {
        let cache = Cache::new(tmp.path().join("credentials.json"));
        cache
            .put_user_key(
                &server.uri(),
                "t-abcd1234",
                CachedUserKey {
                    user_key: "cached-key".to_string(),
                    expires_at: Some(time::OffsetDateTime::now_utc().unix_timestamp() + 3600),
                },
            )
            .unwrap();
        let session = Session::new(server.uri(), "id-token")
            .unwrap()
            .with_cache(cache);
        PlatformApi::from_session(session)
    }

    fn tenant() -> TenantId {
        TenantId("t-abcd1234".to_string())
    }
    fn node() -> NodeId {
        NodeId("n-w2tjezz3".to_string())
    }

    fn desired(name: &str, def: &str, state: DesiredState) -> DesiredPipeline {
        DesiredPipeline {
            name: name.to_string(),
            definition: def.to_string(),
            state,
            node: None,
        }
    }

    fn proxy_path(endpoint: &str) -> String {
        format!("/user/node-proxy/t-abcd1234/n-w2tjezz3/{endpoint}")
    }

    #[tokio::test]
    async fn create_posts_body_and_returns_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(proxy_path("pipeline/create")))
            .and(body_partial_json(
                serde_json::json!({"name": "p", "definition": "version", "hidden": false}),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(include_str!("../../tests/fixtures/pipeline_create.json")),
            )
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let api = api_for(&server, &tmp);
        // Use Stopped so no follow-up transition is needed for this assertion.
        let id = api
            .create(
                &tenant(),
                &node(),
                &desired("p", "version", DesiredState::Stopped),
            )
            .await
            .unwrap();
        assert_eq!(id.as_str(), "4c7f2b11-6169-4d1b-89b4-4fc0a68b3d4a");
    }

    #[tokio::test]
    async fn create_running_drives_start() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(proxy_path("pipeline/create")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(include_str!("../../tests/fixtures/pipeline_create.json")),
            )
            .mount(&server)
            .await;
        // The create→start sequence must POST pipeline/update with action=start.
        Mock::given(method("POST"))
            .and(path(proxy_path("pipeline/update")))
            .and(body_partial_json(serde_json::json!({"action": "start"})))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let api = api_for(&server, &tmp);
        api.create(
            &tenant(),
            &node(),
            &desired("p", "version", DesiredState::Running),
        )
        .await
        .unwrap();
        // `expect(1)` is verified on drop of the server.
    }

    #[tokio::test]
    async fn set_edits_in_place() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(proxy_path("pipeline/update")))
            .and(body_partial_json(
                serde_json::json!({"id": "pid-1", "definition": "v2", "name": "p"}),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(include_str!("../../tests/fixtures/pipeline_update.json")),
            )
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let api = api_for(&server, &tmp);
        api.set(
            &tenant(),
            &node(),
            &PipelineId("pid-1".to_string()),
            &desired("p", "v2", DesiredState::Running),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn delete_posts_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(proxy_path("pipeline/delete")))
            .and(body_partial_json(serde_json::json!({"id": "pid-1"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(include_str!("../../tests/fixtures/pipeline_delete.json")),
            )
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let api = api_for(&server, &tmp);
        api.delete(&tenant(), &node(), &PipelineId("pid-1".to_string()))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn resolve_pipeline_by_name() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(proxy_path("pipeline/list")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(include_str!("../../tests/fixtures/pipeline_list.json")),
            )
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let api = api_for(&server, &tmp);
        let p = api
            .resolve_pipeline(&tenant(), &node(), "wrong-pipeline")
            .await
            .unwrap();
        assert_eq!(p.id.as_str(), "08446737-da9b-4787-8599-97d85c48c3bb");
        // Not-found is an error.
        assert!(
            api.resolve_pipeline(&tenant(), &node(), "nope")
                .await
                .is_err()
        );
    }

    #[test]
    fn proxy_suffix_construction() {
        let suffix = PlatformApi::proxy_suffix(
            &TenantId("t-abcd1234".to_string()),
            &NodeId("n-w2tjezz3".to_string()),
            "pipeline/list",
        );
        assert_eq!(suffix, "node-proxy/t-abcd1234/n-w2tjezz3/pipeline/list");
    }

    #[tokio::test]
    async fn mock_client_lists_and_proxies() {
        let mut nodes = std::collections::HashMap::new();
        nodes.insert(
            "t-abcd1234".to_string(),
            vec![Node {
                node_id: NodeId("n-w2tjezz3".to_string()),
                name: "edge".to_string(),
                connected: true,
                lifecycle_state: None,
            }],
        );
        let mut proxy = std::collections::HashMap::new();
        proxy.insert(
            "pipeline/list".to_string(),
            r#"{"pipelines":[]}"#.to_string(),
        );
        let mock = MockClient {
            workspaces: vec![Workspace {
                tenant_id: TenantId("t-abcd1234".to_string()),
                name: "prod".to_string(),
            }],
            nodes,
            proxy_responses: proxy,
            ..Default::default()
        };

        let ws = mock.list_workspaces().await.unwrap();
        assert_eq!(ws.len(), 1);
        let ns = mock
            .list_nodes(&TenantId("t-abcd1234".to_string()))
            .await
            .unwrap();
        assert_eq!(ns[0].name, "edge");

        let value: serde_json::Value = mock
            .node_proxy(
                &TenantId("t-abcd1234".to_string()),
                &NodeId("n-w2tjezz3".to_string()),
                "pipeline/list",
                &serde_json::json!({}),
            )
            .await
            .unwrap();
        assert!(value.get("pipelines").is_some());
    }

    #[tokio::test]
    async fn list_pipelines_via_node_proxy() {
        use crate::auth::cache::{Cache, CachedUserKey};
        use crate::client::session::Session;
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let fixture = include_str!("../../tests/fixtures/pipeline_list.json");
        Mock::given(method("POST"))
            .and(path("/user/node-proxy/t-abcd1234/n-w2tjezz3/pipeline/list"))
            .and(header("X-Tenzir-UserKey", "cached-key"))
            .respond_with(ResponseTemplate::new(200).set_body_string(fixture))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let cache = Cache::new(tmp.path().join("credentials.json"));
        // Seed a valid key so no switch-tenant round-trip is needed.
        cache
            .put_user_key(
                &server.uri(),
                "t-abcd1234",
                CachedUserKey {
                    user_key: "cached-key".to_string(),
                    expires_at: Some(time::OffsetDateTime::now_utc().unix_timestamp() + 3600),
                },
            )
            .unwrap();
        let session = Session::new(server.uri(), "id-token")
            .unwrap()
            .with_cache(cache);
        let api = PlatformApi::from_session(session);

        let pipelines = api
            .list_pipelines(
                &TenantId("t-abcd1234".to_string()),
                &NodeId("n-w2tjezz3".to_string()),
            )
            .await
            .unwrap();
        assert_eq!(pipelines.len(), 2);
        assert_eq!(pipelines[0].name, "user-assigned-name");
    }

    #[tokio::test]
    async fn node_proxy_maps_410_to_disconnected() {
        use crate::auth::cache::Cache;
        use crate::client::session::Session;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/user/switch-tenant"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"user_key": "k"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/user/node-proxy/t-aaaa1111/n-w2tjezz3/pipeline/list"))
            .respond_with(ResponseTemplate::new(410))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let session = Session::new(server.uri(), "id-token")
            .unwrap()
            .with_cache(Cache::new(tmp.path().join("credentials.json")));
        let api = PlatformApi::from_session(session);
        let err = api
            .node_proxy::<serde_json::Value>(
                &TenantId("t-aaaa1111".to_string()),
                &NodeId("n-w2tjezz3".to_string()),
                "pipeline/list",
                &serde_json::json!({}),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::NodeDisconnected));
    }

    #[tokio::test]
    async fn mock_client_reports_disconnected() {
        let mut disconnected = std::collections::HashSet::new();
        disconnected.insert("pipeline/list".to_string());
        let mock = MockClient {
            disconnected_endpoints: disconnected,
            ..Default::default()
        };
        let err = mock
            .node_proxy::<serde_json::Value>(
                &TenantId("t-abcd1234".to_string()),
                &NodeId("n-w2tjezz3".to_string()),
                "pipeline/list",
                &serde_json::json!({}),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::NodeDisconnected));
    }

    #[tokio::test]
    async fn run_query_paginates_and_stops() {
        let server = MockServer::start().await;
        // Launch succeeds.
        Mock::given(method("POST"))
            .and(path(proxy_path("pipeline/launch")))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        // First serve page: one event + continuation token (matched first,
        // limited to a single response).
        Mock::given(method("POST"))
            .and(path(proxy_path("serve")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "events": [{"n": 1}],
                "next_continuation_token": "tok"
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Subsequent serve page: final event, no token.
        Mock::given(method("POST"))
            .and(path(proxy_path("serve")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "events": [{"n": 2}]
            })))
            .mount(&server)
            .await;
        // Best-effort cleanup stop.
        Mock::given(method("POST"))
            .and(path(proxy_path("pipeline/update")))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let api = api_for(&server, &tmp);
        let events = api
            .run_query(&tenant(), &node(), "diagnostics", 100)
            .await
            .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["n"], 1);
        assert_eq!(events[1]["n"], 2);
    }

    #[tokio::test]
    async fn stream_query_delivers_pages_and_stops() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(proxy_path("pipeline/launch")))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        // First page: one event + token.
        Mock::given(method("POST"))
            .and(path(proxy_path("serve")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "events": [{"data": {"n": 1}}],
                "next_continuation_token": "tok"
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Final page: one event, no token (pipeline completed).
        Mock::given(method("POST"))
            .and(path(proxy_path("serve")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "events": [{"data": {"n": 2}}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(proxy_path("pipeline/update")))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let api = api_for(&server, &tmp);
        let mut seen: Vec<serde_json::Value> = Vec::new();
        api.stream_query(&tenant(), &node(), "version", |events| {
            seen.extend(events.iter().cloned());
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0]["data"]["n"], 1);
        assert_eq!(seen[1]["data"]["n"], 2);
    }

    #[tokio::test]
    async fn stream_query_maps_failed_state_to_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(proxy_path("pipeline/launch")))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(proxy_path("serve")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "events": [],
                "state": "failed"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(proxy_path("pipeline/update")))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let api = api_for(&server, &tmp);
        let err = api
            .stream_query(&tenant(), &node(), "version", |_| Ok(()))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Platform(_)));
    }

    #[tokio::test]
    async fn stream_run_launches_companion_diagnostics_pipeline() {
        use wiremock::matchers::body_string_contains;

        let server = MockServer::start().await;
        // The companion diagnostics pipeline must be launched exactly once.
        Mock::given(method("POST"))
            .and(path(proxy_path("pipeline/launch")))
            .and(body_string_contains("diagnostics live=true, retro=true"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;
        // The main pipeline launch (any other body).
        Mock::given(method("POST"))
            .and(path(proxy_path("pipeline/launch")))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        // Both serve loops complete immediately (no continuation token).
        Mock::given(method("POST"))
            .and(path(proxy_path("serve")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "events": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(proxy_path("pipeline/update")))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let api = api_for(&server, &tmp);
        api.stream_run(&tenant(), &node(), "version", |_| Ok(()), |_| Ok(()))
            .await
            .unwrap();
        // `expect(1)` on the diagnostics launch is verified on server drop.
    }

    #[tokio::test]
    async fn run_query_maps_failed_state_to_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(proxy_path("pipeline/launch")))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(proxy_path("serve")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "events": [],
                "state": "failed"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(proxy_path("pipeline/update")))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let api = api_for(&server, &tmp);
        let err = api
            .run_query(&tenant(), &node(), "diagnostics", 100)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Platform(_)));
    }
}
