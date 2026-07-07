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

/// `generate-client-config` request body.
#[derive(Debug, Serialize)]
struct GenerateClientConfigRequest<'a> {
    tenant_id: &'a str,
    config_type: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_name: Option<&'a str>,
}

/// The node client configuration returned by `generate-client-config`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClientConfig {
    /// The id of the (possibly newly-created) node.
    pub node_id: String,
    /// The suggested filename for the config, if any.
    #[serde(default)]
    pub filename: Option<String>,
    /// The rendered configuration file contents.
    #[serde(default)]
    pub contents: String,
}

/// `delete-node` request body.
#[derive(Debug, Serialize)]
struct DeleteNodeRequest<'a> {
    tenant_id: &'a str,
    node_id: &'a str,
}

/// `workspace/create-invitation` request body.
#[derive(Debug, Serialize)]
struct CreateInvitationRequest<'a> {
    tenant_id: &'a str,
    role: &'a str,
    label: &'a str,
}

/// `workspace/create-invitation` response body.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Invitation {
    /// The id of the created invitation.
    pub invitation_id: String,
    /// The token to redeem the invitation.
    pub token: String,
}

/// `workspace/list-invitations` response body.
#[derive(Debug, Deserialize)]
struct ListInvitationsResponse {
    #[serde(default)]
    invitations: Vec<InvitationInfo>,
}

/// An invitation entry in the list-invitations response.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InvitationInfo {
    /// The id of the invitation.
    pub invitation_id: String,
    /// The status of the invitation, if reported.
    #[serde(default)]
    pub status: Option<String>,
    /// The label attached to the invitation, if any.
    #[serde(default)]
    pub label: Option<String>,
    /// The role granted by the invitation, if reported (org invitations).
    #[serde(default)]
    pub role: Option<String>,
}

/// An organization entry (`org/list`, `org/get`, `org/create`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Organization {
    /// The id of the organization.
    pub organization_id: String,
    /// The name of the organization, if reported.
    #[serde(default)]
    pub name: Option<String>,
}

/// `org/list` response body.
#[derive(Debug, Deserialize)]
struct OrgListResponse {
    #[serde(default)]
    organizations: Vec<Organization>,
}

/// `org/list-members` response body.
#[derive(Debug, Deserialize)]
struct OrgListMembersResponse {
    #[serde(default)]
    members: Vec<serde_json::Value>,
}

/// `org/list-invitations` response body.
#[derive(Debug, Deserialize)]
struct OrgListInvitationsResponse {
    #[serde(default)]
    invitations: Vec<InvitationInfo>,
}

/// Aggregated organization information for `org info`.
#[derive(Debug, Clone, Serialize)]
pub struct OrgInfo {
    /// The organization.
    pub organization: Organization,
    /// The number of members.
    pub members: usize,
    /// The number of pending invitations.
    pub pending_invitations: usize,
}

/// `org/redeem-invitation` response body.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RedeemedOrg {
    /// The name of the joined organization.
    #[serde(default)]
    pub name: Option<String>,
    /// The id of the joined organization.
    pub organization_id: String,
    /// The role granted in the organization.
    #[serde(default)]
    pub role: Option<String>,
}

/// An alert entry (`alert/list`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Alert {
    /// The id of the alert.
    pub id: String,
    /// The monitored node id.
    #[serde(default)]
    pub node_id: Option<String>,
    /// The idle duration (seconds) before the alert triggers.
    #[serde(default)]
    pub duration: Option<f64>,
    /// The webhook URL called when the alert triggers.
    #[serde(default)]
    pub webhook_url: Option<String>,
}

/// `alert/list` response body.
#[derive(Debug, Deserialize)]
struct AlertListResponse {
    #[serde(default)]
    alerts: Vec<Alert>,
}

/// `workspace/redeem-invitation` response body.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RedeemedWorkspace {
    /// The name of the joined workspace.
    #[serde(default)]
    pub name: Option<String>,
    /// The id of the joined workspace.
    pub tenant_id: String,
    /// The role granted in the workspace.
    #[serde(default)]
    pub role: Option<String>,
}

/// `authenticate` response body (generic, non-workspace-scoped `user_key`).
#[derive(Debug, Deserialize)]
struct AuthenticateResponse {
    user_key: String,
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

    /// Generate a node client configuration file (`generate-client-config`).
    ///
    /// Pass `node_id` to download the config for an existing node, or
    /// `node_name` to create a new node.
    pub async fn generate_client_config(
        &self,
        tenant: &TenantId,
        config_type: &str,
        node_id: Option<&str>,
        node_name: Option<&str>,
    ) -> Result<ClientConfig> {
        let req = GenerateClientConfigRequest {
            tenant_id: tenant.as_str(),
            config_type,
            node_id,
            node_name,
        };
        self.with_key_retry(tenant, |key| {
            let req = &req;
            async move {
                self.client
                    .request_user(&key, "generate-client-config", req)
                    .await
            }
        })
        .await
    }

    /// Delete a node by id (`delete-node`).
    pub async fn delete_node(&self, tenant: &TenantId, node_id: &str) -> Result<()> {
        let req = DeleteNodeRequest {
            tenant_id: tenant.as_str(),
            node_id,
        };
        let _: serde_json::Value = self
            .with_key_retry(tenant, |key| {
                let req = &req;
                async move { self.client.request_user(&key, "delete-node", req).await }
            })
            .await?;
        Ok(())
    }

    /// Create an invitation for `tenant` (`workspace/create-invitation`).
    pub async fn create_invitation(
        &self,
        tenant: &TenantId,
        role: &str,
        label: &str,
    ) -> Result<Invitation> {
        let req = CreateInvitationRequest {
            tenant_id: tenant.as_str(),
            role,
            label,
        };
        self.with_key_retry(tenant, |key| {
            let req = &req;
            async move {
                self.client
                    .request_user(&key, "workspace/create-invitation", req)
                    .await
            }
        })
        .await
    }

    /// List invitations for `tenant` (`workspace/list-invitations`).
    pub async fn list_invitations(&self, tenant: &TenantId) -> Result<Vec<InvitationInfo>> {
        let req = serde_json::json!({ "tenant_id": tenant.as_str() });
        let resp: ListInvitationsResponse = self
            .with_key_retry(tenant, |key| {
                let req = &req;
                async move {
                    self.client
                        .request_user(&key, "workspace/list-invitations", req)
                        .await
                }
            })
            .await?;
        Ok(resp.invitations)
    }

    /// Revoke an invitation for `tenant` (`workspace/revoke-invitation`).
    pub async fn revoke_invitation(&self, tenant: &TenantId, invitation_id: &str) -> Result<()> {
        let req = serde_json::json!({
            "tenant_id": tenant.as_str(),
            "invitation_id": invitation_id,
        });
        let _: serde_json::Value = self
            .with_key_retry(tenant, |key| {
                let req = &req;
                async move {
                    self.client
                        .request_user(&key, "workspace/revoke-invitation", req)
                        .await
                }
            })
            .await?;
        Ok(())
    }

    /// Exchange the `id_token` for a generic `user_key` (`authenticate`).
    async fn authenticate(&self) -> Result<String> {
        let req = LoginInfoRequest {
            id_token: &self.id_token,
        };
        let resp: AuthenticateResponse = self.client.request_public("authenticate", &req).await?;
        Ok(resp.user_key)
    }

    /// Make a `USER` request authenticated with a generic (non-workspace) key.
    async fn org_request<T: serde::de::DeserializeOwned>(
        &self,
        suffix: &str,
        body: &impl Serialize,
    ) -> Result<T> {
        let key = self.authenticate().await?;
        self.client.request_user(&key, suffix, body).await
    }

    /// List the organizations the user belongs to (`org/list`).
    pub async fn org_list(&self) -> Result<Vec<Organization>> {
        let resp: OrgListResponse = self.org_request("org/list", &serde_json::json!({})).await?;
        Ok(resp.organizations)
    }

    /// Return the id of the user's current (first) organization.
    async fn current_org_id(&self) -> Result<String> {
        let organizations = self.org_list().await?;
        organizations
            .into_iter()
            .next()
            .map(|o| o.organization_id)
            .ok_or_else(|| {
                Error::Platform("you are not a member of any organization".to_string())
            })
    }

    /// Create an organization (`org/create`).
    pub async fn org_create(&self, name: &str) -> Result<Organization> {
        self.org_request("org/create", &serde_json::json!({ "name": name }))
            .await
    }

    /// Create an org-owned workspace (`workspace/create`); returns its id.
    pub async fn org_create_workspace(&self, name: &str) -> Result<String> {
        let resp: serde_json::Value = self
            .org_request(
                "workspace/create",
                &serde_json::json!({ "name": name, "org_owned": true }),
            )
            .await?;
        resp.get("tenant_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| Error::Platform("missing tenant_id in response".to_string()))
    }

    /// Fetch aggregated information about the current organization.
    pub async fn org_info(&self) -> Result<OrgInfo> {
        let org_id = self.current_org_id().await?;
        let body = serde_json::json!({ "organization_id": org_id });
        let organization: Organization = self.org_request("org/get", &body).await?;
        let members: OrgListMembersResponse = self.org_request("org/list-members", &body).await?;
        // Listing invitations may be forbidden for non-admins; tolerate 403.
        let invitations = match self
            .org_request::<OrgListInvitationsResponse>("org/list-invitations", &body)
            .await
        {
            Ok(resp) => resp.invitations,
            Err(Error::Auth(_)) => Vec::new(),
            Err(e) => return Err(e),
        };
        let pending = invitations
            .iter()
            .filter(|i| i.status.as_deref() == Some("pending"))
            .count();
        Ok(OrgInfo {
            organization,
            members: members.members.len(),
            pending_invitations: pending,
        })
    }

    /// Create an invitation for the current organization.
    pub async fn org_create_invitation(&self, role: &str, label: &str) -> Result<Invitation> {
        let org_id = self.current_org_id().await?;
        self.org_request(
            "org/create-invitation",
            &serde_json::json!({
                "organization_id": org_id,
                "role": role,
                "label": label,
            }),
        )
        .await
    }

    /// List invitations for the current organization.
    pub async fn org_list_invitations(&self) -> Result<Vec<InvitationInfo>> {
        let org_id = self.current_org_id().await?;
        let resp: OrgListInvitationsResponse = self
            .org_request(
                "org/list-invitations",
                &serde_json::json!({ "organization_id": org_id }),
            )
            .await?;
        Ok(resp.invitations)
    }

    /// Revoke an invitation for the current organization.
    pub async fn org_revoke_invitation(&self, invitation_id: &str) -> Result<()> {
        let org_id = self.current_org_id().await?;
        let _: serde_json::Value = self
            .org_request(
                "org/revoke-invitation",
                &serde_json::json!({
                    "organization_id": org_id,
                    "invitation_id": invitation_id,
                }),
            )
            .await?;
        Ok(())
    }

    /// Redeem an organization invitation token.
    pub async fn org_redeem_invitation(&self, token: &str) -> Result<RedeemedOrg> {
        self.org_request("org/redeem-invitation", &serde_json::json!({ "token": token }))
            .await
    }

    /// Delete the current organization.
    pub async fn org_delete(&self) -> Result<String> {
        let org_id = self.current_org_id().await?;
        let _: serde_json::Value = self
            .org_request("org/delete", &serde_json::json!({ "organization_id": org_id }))
            .await?;
        Ok(org_id)
    }

    /// Leave the current organization.
    pub async fn org_leave(&self) -> Result<String> {
        let org_id = self.current_org_id().await?;
        let _: serde_json::Value = self
            .org_request("org/leave", &serde_json::json!({ "organization_id": org_id }))
            .await?;
        Ok(org_id)
    }

    /// Remove a member from the current organization.
    pub async fn org_remove_member(&self, user_id: &str) -> Result<()> {
        let org_id = self.current_org_id().await?;
        let _: serde_json::Value = self
            .org_request(
                "org/remove-member",
                &serde_json::json!({
                    "organization_id": org_id,
                    "user_id": user_id,
                }),
            )
            .await?;
        Ok(())
    }

    /// Add an alert to a workspace (`alert/add`).
    pub async fn alert_add(
        &self,
        tenant: &TenantId,
        node_id: &str,
        duration_seconds: u64,
        webhook_url: &str,
        webhook_body: &str,
    ) -> Result<serde_json::Value> {
        let req = serde_json::json!({
            "tenant_id": tenant.as_str(),
            "node_id": node_id,
            "duration": duration_seconds,
            "webhook_url": webhook_url,
            "webhook_body": webhook_body,
        });
        self.with_key_retry(tenant, |key| {
            let req = &req;
            async move { self.client.request_user(&key, "alert/add", req).await }
        })
        .await
    }

    /// Delete an alert (`alert/delete`).
    pub async fn alert_delete(&self, tenant: &TenantId, alert_id: &str) -> Result<()> {
        let req = serde_json::json!({
            "tenant_id": tenant.as_str(),
            "alert_id": alert_id,
        });
        let _: serde_json::Value = self
            .with_key_retry(tenant, |key| {
                let req = &req;
                async move { self.client.request_user(&key, "alert/delete", req).await }
            })
            .await?;
        Ok(())
    }

    /// List alerts for a workspace (`alert/list`).
    pub async fn alert_list(&self, tenant: &TenantId) -> Result<Vec<Alert>> {
        let req = serde_json::json!({ "tenant_id": tenant.as_str() });
        let resp: AlertListResponse = self
            .with_key_retry(tenant, |key| {
                let req = &req;
                async move { self.client.request_user(&key, "alert/list", req).await }
            })
            .await?;
        Ok(resp.alerts)
    }

    /// Redeem an invitation token (`workspace/redeem-invitation`).
    pub async fn redeem_invitation(&self, token: &str) -> Result<RedeemedWorkspace> {
        let user_key = self.authenticate().await?;
        let req = serde_json::json!({ "token": token });
        self.client
            .request_user(&user_key, "workspace/redeem-invitation", &req)
            .await
    }

    /// Rename a workspace (`rename-tenant`, public endpoint).
    pub async fn rename_workspace(&self, tenant: &TenantId, name: &str) -> Result<()> {
        let req = serde_json::json!({
            "tenant_id": tenant.as_str(),
            "name": name,
        });
        let _: serde_json::Value = self.client.request_public("rename-tenant", &req).await?;
        Ok(())
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
