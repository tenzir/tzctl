//! Handlers for `tz workspace list` and `tz workspace select`.

use owo_colors::OwoColorize;

use crate::auth::TokenSources;
use crate::client::PlatformClient;
use crate::config::ResolvedConfig;
use crate::error::{Error, HintedError};
use crate::model::{self, TenantId};
use crate::output::{self, OutputMode};
use crate::table::{Align, Table};

/// Resolve the current workspace (tenant) from config, with a hinted error.
pub(super) fn current_workspace(config: &ResolvedConfig) -> Result<TenantId, HintedError> {
    config
        .workspace
        .clone()
        .map(TenantId::from)
        .ok_or_else(|| {
            HintedError::new(Error::Config("no workspace configured".to_string())).with_hint(
                "set `[workspace] id` in tenzir.toml, pass --workspace, or run \
                 `tz workspace select`",
            )
        })
}

/// Handle `tz workspace list`.
pub async fn list(
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
) -> Result<(), HintedError> {
    let client = super::platform_client(config, sources).await?;
    let workspaces = client.list_workspaces().await.map_err(HintedError::new)?;
    output::render(output, &workspaces, || render_table(&workspaces))
        .map_err(|e| HintedError::new(Error::Other(e.into())))?;
    Ok(())
}

/// Render the workspace list as a numbered table.
fn render_table(workspaces: &[crate::model::Workspace]) -> String {
    if workspaces.is_empty() {
        return "No workspaces available.".to_string();
    }
    let mut table = Table::new(["#", "NAME", "WORKSPACE ID"]).align(0, Align::Right);
    for (i, ws) in workspaces.iter().enumerate() {
        table.row([
            (i + 1).to_string(),
            ws.name.clone(),
            ws.tenant_id.to_string(),
        ]);
    }
    table.render()
}

/// Handle `tz workspace select <query>`.
pub async fn select(
    config: &ResolvedConfig,
    sources: TokenSources,
    query: &str,
) -> Result<(), HintedError> {
    let client = super::platform_client(config, sources).await?;
    let workspaces = client.list_workspaces().await.map_err(HintedError::new)?;
    let index = model::resolve_index(
        &workspaces,
        query,
        |w| w.tenant_id.as_str(),
        |w| w.name.as_str(),
    )
    .map_err(|e| {
        HintedError::new(Error::Config(format!("cannot select workspace: {e}")))
            .with_hint("run `tz workspace list` to see available workspaces")
    })?;
    let chosen = &workspaces[index];
    let tenant: &TenantId = &chosen.tenant_id;

    // Mint and cache the workspace key so subsequent commands work offline.
    client
        .session()
        .user_key(tenant)
        .await
        .map_err(HintedError::new)?;

    println!(
        "{} Selected workspace {} ({})",
        crate::symbols::OK.green().bold(),
        chosen.name.bold(),
        tenant
    );
    println!(
        "hint: set `[workspace] id = \"{tenant}\"` in tenzir.toml (or use --workspace {tenant})"
    );
    Ok(())
}

/// Handle `tz workspace invite`.
pub async fn invite(
    config: &ResolvedConfig,
    sources: TokenSources,
    role: &str,
    label: Option<&str>,
) -> Result<(), HintedError> {
    if role != "member" && role != "admin" {
        return Err(HintedError::new(Error::Config(format!(
            "invalid role {role:?}: must be 'member' or 'admin'"
        ))));
    }
    let tenant = current_workspace(config)?;
    let client = super::platform_client(config, sources).await?;
    let invitation = client
        .create_invitation(&tenant, role, label.unwrap_or(""))
        .await
        .map_err(HintedError::new)?;
    println!(
        "{} Created invitation {}",
        crate::symbols::OK.green().bold(),
        invitation.invitation_id.bold()
    );
    println!("token: {}", invitation.token);
    Ok(())
}

/// Handle `tz workspace list-invitations`.
pub async fn list_invitations(
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
) -> Result<(), HintedError> {
    let tenant = current_workspace(config)?;
    let client = super::platform_client(config, sources).await?;
    let invitations = client
        .list_invitations(&tenant)
        .await
        .map_err(HintedError::new)?;
    output::render(output, &invitations, || render_invitations(&invitations))
        .map_err(|e| HintedError::new(Error::Other(e.into())))?;
    Ok(())
}

/// Render the invitation list as a table.
fn render_invitations(invitations: &[crate::client::InvitationInfo]) -> String {
    if invitations.is_empty() {
        return "No invitations.".to_string();
    }
    let mut table = Table::new(["INVITATION ID", "STATUS", "LABEL"]);
    for inv in invitations {
        table.row([
            inv.invitation_id.clone(),
            inv.status.clone().unwrap_or_default(),
            inv.label.clone().unwrap_or_default(),
        ]);
    }
    table.render()
}

/// Handle `tz workspace revoke-invitation <invitation_id>`.
pub async fn revoke_invitation(
    config: &ResolvedConfig,
    sources: TokenSources,
    invitation_id: &str,
) -> Result<(), HintedError> {
    let tenant = current_workspace(config)?;
    let client = super::platform_client(config, sources).await?;
    client
        .revoke_invitation(&tenant, invitation_id)
        .await
        .map_err(HintedError::new)?;
    println!(
        "{} Revoked invitation {}",
        crate::symbols::OK.green().bold(),
        invitation_id.bold()
    );
    Ok(())
}

/// Handle `tz workspace redeem-invitation <token>`.
pub async fn redeem_invitation(
    config: &ResolvedConfig,
    sources: TokenSources,
    token: &str,
) -> Result<(), HintedError> {
    let client = super::platform_client(config, sources).await?;
    let workspace = client.redeem_invitation(token).await.map_err(HintedError::new)?;
    let name = workspace.name.as_deref().unwrap_or(&workspace.tenant_id);
    let role = workspace.role.as_deref().unwrap_or("member");
    println!(
        "{} Joined workspace {} ({}) as {}",
        crate::symbols::OK.green().bold(),
        name.bold(),
        workspace.tenant_id,
        role
    );
    Ok(())
}

/// Handle `tz workspace rename <name>`.
pub async fn rename(
    config: &ResolvedConfig,
    sources: TokenSources,
    name: &str,
) -> Result<(), HintedError> {
    let tenant = current_workspace(config)?;
    let client = super::platform_client(config, sources).await?;
    client
        .rename_workspace(&tenant, name)
        .await
        .map_err(HintedError::new)?;
    println!(
        "{} Renamed workspace {} to {}",
        crate::symbols::OK.green().bold(),
        tenant,
        name.bold()
    );
    Ok(())
}
