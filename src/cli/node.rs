//! Handler for `tz node list`.

use comfy_table::{Table, presets::UTF8_FULL};
use owo_colors::OwoColorize;

use crate::auth::TokenSources;
use crate::client::PlatformClient;
use crate::config::ResolvedConfig;
use crate::error::{Error, HintedError};
use crate::model::{Node, TenantId};
use crate::output::{self, OutputMode};

/// Handle `tz node list`.
pub async fn list(
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
) -> Result<(), HintedError> {
    let tenant = config
        .workspace
        .clone()
        .map(TenantId::from)
        .ok_or_else(|| {
            HintedError::new(Error::Config("no workspace configured".to_string())).with_hint(
                "set `[workspace] id` in tenzir.toml, pass --workspace, or run \
                 `tz workspace select`",
            )
        })?;

    let client = super::platform_client(config, sources).await?;
    let nodes = client.list_nodes(&tenant).await.map_err(HintedError::new)?;
    output::render(output, &nodes, || render_table(&nodes))
        .map_err(|e| HintedError::new(Error::Other(e.into())))?;
    Ok(())
}

/// Render the node list as a numbered table with a connection indicator.
fn render_table(nodes: &[Node]) -> String {
    if nodes.is_empty() {
        return "No nodes in this workspace.".to_string();
    }
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(["#", "NAME", "NODE ID", "STATUS"]);
    for (i, node) in nodes.iter().enumerate() {
        let status = if node.connected {
            "connected".green().to_string()
        } else {
            "disconnected".red().to_string()
        };
        table.add_row([
            (i + 1).to_string(),
            node.name.clone(),
            node.node_id.to_string(),
            status,
        ]);
    }
    table.to_string()
}
