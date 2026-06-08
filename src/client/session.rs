//! Workspace key exchange and the `user_key` cache.

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::transport::AppClient;
use crate::auth::cache::{Cache, CachedUserKey};
use crate::auth::token;
use crate::error::{Error, Result};
use crate::model::{Node, TenantId, Workspace};

/// The default requested lifetime for a minted `user_key` (~15 minutes).
pub const DEFAULT_KEY_LIFETIME_SECONDS: i64 = 900;

// --- Wire types ------------------------------------------------------------

/// `get-login-info` request body.
#[derive(Debug, Serialize)]
struct LoginInfoRequest<'a> {
    id_token: &'a str,
}

/// `get-login-info` response body.
#[derive(Debug, Deserialize)]
struct LoginInfoResponse {
    #[serde(default)]
    allowed_tenants: Vec<TenantInfo>,
}

/// A workspace entry in the login-info response.
#[derive(Debug, Deserialize)]
struct TenantInfo {
    tenant_id: String,
    #[serde(default)]
    name: Option<String>,
}

/// `switch-tenant` request body.
#[derive(Debug, Serialize)]
struct SwitchTenantRequest<'a> {
    id_token: &'a str,
    tenant_id: &'a str,
    requested_lifetime_seconds: i64,
}

/// `switch-tenant` response body.
#[derive(Debug, Deserialize)]
struct SwitchTenantResponse {
    user_key: String,
}

/// `list-nodes` request body.
#[derive(Debug, Serialize)]
struct ListNodesRequest<'a> {
    tenant_id: &'a str,
}

/// `list-nodes` response body.
#[derive(Debug, Deserialize)]
struct ListNodesResponse {
    #[serde(default)]
    nodes: Vec<RawNode>,
}

/// A node entry in the list-nodes response (defensively typed).
#[derive(Debug, Deserialize)]
struct RawNode {
    node_id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    connected: Option<bool>,
}

impl RawNode {
    /// Convert into the domain [`Node`], deriving the connection status.
    fn into_node(self) -> Node {
        // Prefer an explicit `connected` flag; otherwise infer from the
        // lifecycle state. (Heuristic — verify against the platform spec.)
        let connected = self.connected.unwrap_or_else(|| {
            self.lifecycle_state
                .as_deref()
                .map(|s| {
                    let s = s.to_ascii_lowercase();
                    s == "connected" || s == "running" || s == "online"
                })
                .unwrap_or(false)
        });
        let name = self.name.unwrap_or_else(|| self.node_id.clone());
        Node {
            node_id: self.node_id.into(),
            name,
            connected,
            lifecycle_state: self.lifecycle_state,
        }
    }
}

// --- Session ---------------------------------------------------------------

/// Holds the `id_token`, the transport, and the `user_key` cache; mints and
/// re-mints workspace keys on demand.
pub struct Session {
    client: AppClient,
    cache: Cache,
    api_endpoint: String,
    id_token: String,
    lifetime_seconds: i64,
    /// Serializes key minting so concurrent calls don't race.
    mint_lock: Mutex<()>,
}

impl Session {
    /// Build a session for `api_endpoint` using the given `id_token`.
    pub fn new(api_endpoint: impl Into<String>, id_token: impl Into<String>) -> Result<Self> {
        let api_endpoint = api_endpoint.into();
        let client = AppClient::new(api_endpoint.clone())?;
        Ok(Self {
            client,
            cache: Cache::default(),
            api_endpoint,
            id_token: id_token.into(),
            lifetime_seconds: DEFAULT_KEY_LIFETIME_SECONDS,
            mint_lock: Mutex::new(()),
        })
    }

    /// Override the credential cache (used in tests).
    #[cfg(test)]
    pub fn with_cache(mut self, cache: Cache) -> Self {
        self.cache = cache;
        self
    }

    /// The underlying transport.
    #[allow(dead_code)] // used by the node-proxy path (stage 4+).
    pub fn client(&self) -> &AppClient {
        &self.client
    }

    /// List the workspaces the user may access (`get-login-info`).
    pub async fn login_info(&self) -> Result<Vec<Workspace>> {
        let req = LoginInfoRequest {
            id_token: &self.id_token,
        };
        let resp: LoginInfoResponse = self.client.request_public("get-login-info", &req).await?;
        Ok(resp
            .allowed_tenants
            .into_iter()
            .map(|t| Workspace {
                name: t.name.unwrap_or_else(|| t.tenant_id.clone()),
                tenant_id: TenantId(t.tenant_id),
            })
            .collect())
    }

    /// Exchange the `id_token` for a workspace-scoped `user_key`.
    pub async fn switch_tenant(&self, tenant: &TenantId) -> Result<String> {
        let req = SwitchTenantRequest {
            id_token: &self.id_token,
            tenant_id: tenant.as_str(),
            requested_lifetime_seconds: self.lifetime_seconds,
        };
        let resp: SwitchTenantResponse = self.client.request_public("switch-tenant", &req).await?;
        Ok(resp.user_key)
    }

    /// Return a usable `user_key` for `tenant`, minting and caching if needed.
    pub async fn user_key(&self, tenant: &TenantId) -> Result<String> {
        if let Some(cached) = self
            .cache
            .get_user_key(&self.api_endpoint, tenant.as_str())?
        {
            let expired = cached
                .expires_at
                .is_some_and(|exp| token::is_expired(exp, 60));
            if !expired {
                return Ok(cached.user_key);
            }
        }
        self.mint_user_key(tenant).await
    }

    /// Force-mint a fresh `user_key` and cache it.
    async fn mint_user_key(&self, tenant: &TenantId) -> Result<String> {
        let _guard = self.mint_lock.lock().await;
        let user_key = self.switch_tenant(tenant).await?;
        let expires_at =
            Some(time::OffsetDateTime::now_utc().unix_timestamp() + self.lifetime_seconds);
        self.cache.put_user_key(
            &self.api_endpoint,
            tenant.as_str(),
            CachedUserKey {
                user_key: user_key.clone(),
                expires_at,
            },
        )?;
        Ok(user_key)
    }

    /// List nodes in `tenant`, ensuring a valid `user_key`.
    pub async fn list_nodes(&self, tenant: &TenantId) -> Result<Vec<Node>> {
        let req = ListNodesRequest {
            tenant_id: tenant.as_str(),
        };
        let resp: ListNodesResponse = self
            .with_key_retry(tenant, |key| {
                let req = &req;
                async move { self.client.request_user(&key, "list-nodes", req).await }
            })
            .await?;
        Ok(resp.nodes.into_iter().map(RawNode::into_node).collect())
    }

    /// Run `op` with a cached key; on a `401` Auth error, re-mint once.
    ///
    /// The key is passed by value so `op` may move it into a returned future.
    pub async fn with_key_retry<T, F, Fut>(&self, tenant: &TenantId, op: F) -> Result<T>
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let key = self.user_key(tenant).await?;
        match op(key).await {
            Err(Error::Auth(_)) => {
                let fresh = self.mint_user_key(tenant).await?;
                op(fresh).await
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn session_for(server: &MockServer, tmp: &std::path::Path) -> Session {
        Session::new(server.uri(), "id-token-xyz")
            .unwrap()
            .with_cache(Cache::new(tmp.join("credentials.json")))
    }

    #[tokio::test]
    async fn login_info_lists_workspaces() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/user/get-login-info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "allowed_tenants": [
                    {"tenant_id": "t-aaaa1111", "name": "prod"},
                    {"tenant_id": "t-bbbb2222"}
                ]
            })))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let session = session_for(&server, tmp.path());
        let ws = session.login_info().await.unwrap();
        assert_eq!(ws.len(), 2);
        assert_eq!(ws[0].name, "prod");
        // Missing name falls back to the tenant id.
        assert_eq!(ws[1].name, "t-bbbb2222");
    }

    #[tokio::test]
    async fn switch_tenant_then_list_nodes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/user/switch-tenant"))
            .and(body_string_contains("t-aaaa1111"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"user_key": "key-1"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/user/list-nodes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "nodes": [
                    {"node_id": "n-w2tjezz3", "name": "edge", "lifecycle_state": "running"}
                ]
            })))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let session = session_for(&server, tmp.path());
        let tenant = TenantId("t-aaaa1111".to_string());
        let nodes = session.list_nodes(&tenant).await.unwrap();
        assert_eq!(nodes[0].name, "edge");
        assert!(nodes[0].connected);
        // The key was cached.
        let cached = session
            .cache
            .get_user_key(&server.uri(), "t-aaaa1111")
            .unwrap()
            .unwrap();
        assert_eq!(cached.user_key, "key-1");
    }

    #[tokio::test]
    async fn expired_key_is_reminted_on_401() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/user/switch-tenant"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"user_key": "fresh-key"})),
            )
            .mount(&server)
            .await;
        // First list-nodes call returns 401; after re-mint it succeeds.
        Mock::given(method("POST"))
            .and(path("/user/list-nodes"))
            .respond_with(ResponseTemplate::new(401).set_body_string("expired"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/user/list-nodes"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"nodes": []})),
            )
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        // Seed an already-expired cached key so the first attempt uses it.
        let cache = Cache::new(tmp.path().join("credentials.json"));
        cache
            .put_user_key(
                &server.uri(),
                "t-aaaa1111",
                CachedUserKey {
                    user_key: "stale-key".to_string(),
                    expires_at: Some(0),
                },
            )
            .unwrap();
        let session = Session::new(server.uri(), "id-token")
            .unwrap()
            .with_cache(cache);
        let tenant = TenantId("t-aaaa1111".to_string());
        let nodes = session.list_nodes(&tenant).await.unwrap();
        assert!(nodes.is_empty());
    }

    #[test]
    fn raw_node_connection_inference() {
        let n = RawNode {
            node_id: "n-aaaa1111".to_string(),
            name: Some("edge".to_string()),
            lifecycle_state: Some("running".to_string()),
            connected: None,
        }
        .into_node();
        assert!(n.connected);

        let n = RawNode {
            node_id: "n-bbbb2222".to_string(),
            name: None,
            lifecycle_state: Some("disconnected".to_string()),
            connected: None,
        }
        .into_node();
        assert!(!n.connected);
        // Name falls back to the node id.
        assert_eq!(n.name, "n-bbbb2222");

        let n = RawNode {
            node_id: "n-cccc3333".to_string(),
            name: None,
            lifecycle_state: Some("running".to_string()),
            connected: Some(false),
        }
        .into_node();
        // Explicit flag wins over the lifecycle heuristic.
        assert!(!n.connected);
    }
}
