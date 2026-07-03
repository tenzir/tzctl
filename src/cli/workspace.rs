//! Handlers for `tz workspace list` and `tz workspace select`.

use owo_colors::OwoColorize;

use crate::auth::TokenSources;
use crate::client::PlatformClient;
use crate::config::ResolvedConfig;
use crate::error::{Error, HintedError};
use crate::model::{self, TenantId};
use crate::output::{self, OutputMode};
use crate::table::{Align, Table};

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
