//! Handler for `tz run <file>` — run a `.tql` file on the target node and
//! stream its results to stdout as newline-delimited JSON.

use std::io::Write;
use std::path::Path;

use crate::auth::TokenSources;
use crate::client::PlatformClient;
use crate::config::ResolvedConfig;
use crate::error::{Error, HintedError};
use crate::output::OutputMode;
use crate::project;

/// Handle `tz run <file>`.
///
/// Reads the pipeline definition from `file` (stripping any frontmatter),
/// launches it as a hidden, short-lived pipeline on the target node, and
/// streams each served event to stdout as a compact JSON line. Streaming stops
/// when the pipeline completes or on Ctrl-C.
pub async fn run(
    config: &ResolvedConfig,
    sources: TokenSources,
    _output: OutputMode,
    file: &Path,
) -> Result<(), HintedError> {
    let (workspace, node) = super::resolve_target(config)?;
    let desired = project::desired_from_file(file).map_err(HintedError::new)?;
    let client = super::platform_client(config, sources).await?;

    let stdout = std::io::stdout();
    client
        .stream_query(&workspace, &node, &desired.definition, |events| {
            let mut out = stdout.lock();
            for event in events {
                // Serve wraps each result as `{schema_id, data}`; unwrap `data`
                // when present, otherwise print the event verbatim.
                let value = event.get("data").unwrap_or(event);
                let line =
                    serde_json::to_string(value).map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
                writeln!(out, "{line}").map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
            }
            out.flush().map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
            Ok(())
        })
        .await
        .map_err(super::list::disconnected_hint)?;

    Ok(())
}
