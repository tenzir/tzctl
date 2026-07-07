//! Handlers for the `tz org` command group.

use owo_colors::OwoColorize;

use crate::auth::TokenSources;
use crate::config::ResolvedConfig;
use crate::error::{Error, HintedError};
use crate::output::{self, OutputMode};
use crate::table::Table;

/// A UTC timestamp suffix like `20260707T120000Z` for default names.
fn timestamp_suffix() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        now.year(),
        now.month() as u8,
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

/// Validate an invitation role.
fn check_role(role: &str) -> Result<(), HintedError> {
    if role != "member" && role != "admin" {
        return Err(HintedError::new(Error::Config(format!(
            "invalid role {role:?}: must be 'member' or 'admin'"
        ))));
    }
    Ok(())
}

/// Handle `tz org info`.
pub async fn info(config: &ResolvedConfig, sources: TokenSources) -> Result<(), HintedError> {
    let client = super::platform_client(config, sources).await?;
    let info = client.session().org_info().await.map_err(HintedError::new)?;
    output::render(OutputMode::Text, &info, || {
        let name = info
            .organization
            .name
            .as_deref()
            .unwrap_or(&info.organization.organization_id);
        format!(
            "Organization: {} ({})\nMembers: {}\nPending invitations: {}",
            name, info.organization.organization_id, info.members, info.pending_invitations
        )
    })
    .map_err(|e| HintedError::new(Error::Other(e.into())))?;
    Ok(())
}

/// Handle `tz org create <name>`.
pub async fn create(
    config: &ResolvedConfig,
    sources: TokenSources,
    name: &str,
) -> Result<(), HintedError> {
    let client = super::platform_client(config, sources).await?;
    let org = client
        .session()
        .org_create(name)
        .await
        .map_err(HintedError::new)?;
    println!(
        "{} Created organization {}",
        crate::symbols::OK.green().bold(),
        org.organization_id.bold()
    );
    Ok(())
}

/// Handle `tz org create-workspace [--name]`.
pub async fn create_workspace(
    config: &ResolvedConfig,
    sources: TokenSources,
    name: Option<&str>,
) -> Result<(), HintedError> {
    let default = format!("workspace-{}", timestamp_suffix());
    let name = name.unwrap_or(&default);
    let client = super::platform_client(config, sources).await?;
    let tenant_id = client
        .session()
        .org_create_workspace(name)
        .await
        .map_err(HintedError::new)?;
    println!(
        "{} Created workspace {}",
        crate::symbols::OK.green().bold(),
        tenant_id.bold()
    );
    Ok(())
}

/// Handle `tz org delete`.
pub async fn delete(
    config: &ResolvedConfig,
    sources: TokenSources,
    yes: bool,
) -> Result<(), HintedError> {
    if !yes && !super::confirm("Delete the current organization?")? {
        println!("Aborted.");
        return Ok(());
    }
    let client = super::platform_client(config, sources).await?;
    let org_id = client
        .session()
        .org_delete()
        .await
        .map_err(HintedError::new)?;
    println!(
        "{} Deleted organization {}",
        crate::symbols::OK.green().bold(),
        org_id.bold()
    );
    Ok(())
}

/// Handle `tz org invite`.
pub async fn invite(
    config: &ResolvedConfig,
    sources: TokenSources,
    role: &str,
    label: Option<&str>,
) -> Result<(), HintedError> {
    check_role(role)?;
    let client = super::platform_client(config, sources).await?;
    let invitation = client
        .session()
        .org_create_invitation(role, label.unwrap_or(""))
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

/// Handle `tz org leave`.
pub async fn leave(
    config: &ResolvedConfig,
    sources: TokenSources,
    yes: bool,
) -> Result<(), HintedError> {
    if !yes && !super::confirm("Leave the current organization?")? {
        println!("Aborted.");
        return Ok(());
    }
    let client = super::platform_client(config, sources).await?;
    let org_id = client.session().org_leave().await.map_err(HintedError::new)?;
    println!(
        "{} Left organization {}",
        crate::symbols::OK.green().bold(),
        org_id.bold()
    );
    Ok(())
}

/// Handle `tz org list-invitations`.
pub async fn list_invitations(
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
) -> Result<(), HintedError> {
    let client = super::platform_client(config, sources).await?;
    let invitations = client
        .session()
        .org_list_invitations()
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
    let mut table = Table::new(["INVITATION ID", "STATUS", "ROLE", "LABEL"]);
    for inv in invitations {
        table.row([
            inv.invitation_id.clone(),
            inv.status.clone().unwrap_or_default(),
            inv.role.clone().unwrap_or_default(),
            inv.label.clone().unwrap_or_default(),
        ]);
    }
    table.render()
}

/// Handle `tz org revoke-invitation <invitation_id>`.
pub async fn revoke_invitation(
    config: &ResolvedConfig,
    sources: TokenSources,
    invitation_id: &str,
) -> Result<(), HintedError> {
    let client = super::platform_client(config, sources).await?;
    client
        .session()
        .org_revoke_invitation(invitation_id)
        .await
        .map_err(HintedError::new)?;
    println!(
        "{} Revoked invitation {}",
        crate::symbols::OK.green().bold(),
        invitation_id.bold()
    );
    Ok(())
}

/// Handle `tz org redeem-invitation <token>`.
pub async fn redeem_invitation(
    config: &ResolvedConfig,
    sources: TokenSources,
    token: &str,
) -> Result<(), HintedError> {
    let client = super::platform_client(config, sources).await?;
    let org = client
        .session()
        .org_redeem_invitation(token)
        .await
        .map_err(HintedError::new)?;
    let name = org.name.as_deref().unwrap_or(&org.organization_id);
    let role = org.role.as_deref().unwrap_or("member");
    println!(
        "{} Joined organization {} ({}) as {}",
        crate::symbols::OK.green().bold(),
        name.bold(),
        org.organization_id,
        role
    );
    Ok(())
}

/// Handle `tz org remove-member <user_id>`.
pub async fn remove_member(
    config: &ResolvedConfig,
    sources: TokenSources,
    user_id: &str,
    yes: bool,
) -> Result<(), HintedError> {
    if !yes && !super::confirm(&format!("Remove member {user_id}?"))? {
        println!("Aborted.");
        return Ok(());
    }
    let client = super::platform_client(config, sources).await?;
    client
        .session()
        .org_remove_member(user_id)
        .await
        .map_err(HintedError::new)?;
    println!(
        "{} Removed member {}",
        crate::symbols::OK.green().bold(),
        user_id.bold()
    );
    Ok(())
}
