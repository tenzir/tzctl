//! Handler for `tz pipeline list` — the plain pipeline inventory.

use comfy_table::{Table, presets::UTF8_FULL};

use crate::auth::TokenSources;
use crate::client::PlatformClient;
use crate::config::ResolvedConfig;
use crate::error::{Error, HintedError};
use crate::model::RemotePipeline;
use crate::output::{self, OutputMode};

/// The maximum definition width shown in the text table.
const DEFINITION_WIDTH: usize = 60;

/// Handle `tz pipeline list`.
pub async fn run(
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
) -> Result<(), HintedError> {
    let (workspace, node) = super::resolve_target(config)?;
    let client = super::platform_client(config, sources).await?;
    let pipelines = client
        .list_pipelines(&workspace, &node)
        .await
        .map_err(disconnected_hint)?;
    output::render(output, &pipelines, || render_table(&pipelines))
        .map_err(|e| HintedError::new(Error::Other(e.into())))?;
    Ok(())
}

/// Map a node-disconnected error to a friendlier hint.
pub(super) fn disconnected_hint(error: Error) -> HintedError {
    match error {
        Error::NodeDisconnected => HintedError::new(error)
            .with_hint("the node is not connected to the platform; check that it is running"),
        other => HintedError::new(other),
    }
}

/// Truncate a definition to a single, width-limited line.
pub(super) fn truncate_definition(definition: &str) -> String {
    let oneline: String = definition.split_whitespace().collect::<Vec<_>>().join(" ");
    if oneline.chars().count() > DEFINITION_WIDTH {
        let clipped: String = oneline.chars().take(DEFINITION_WIDTH - 1).collect();
        format!("{clipped}…")
    } else {
        oneline
    }
}

/// Render pipelines as a plain table.
fn render_table(pipelines: &[RemotePipeline]) -> String {
    if pipelines.is_empty() {
        return "No pipelines on this node.".to_string();
    }
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(["NAME", "STATE", "DEFINITION"]);
    for p in pipelines {
        table.add_row([
            p.name.clone(),
            p.state.to_string(),
            truncate_definition(&p.definition),
        ]);
    }
    table.to_string()
}
