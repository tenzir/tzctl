//! Handler for `tz run <file>` — run a `.tql` file on the target node and
//! stream its results to stdout as newline-delimited JSON, while surfacing any
//! diagnostics (errors/warnings) emitted during execution on stderr.

use std::io::Write;
use std::path::Path;

use owo_colors::OwoColorize;

use crate::auth::TokenSources;
use crate::client::PlatformClient;
use crate::config::ResolvedConfig;
use crate::error::{Error, HintedError};
use crate::output::OutputMode;
use crate::project;
use crate::status::{Diagnostic, Severity};

/// Handle `tz run <file>`.
///
/// Reads the pipeline definition from `file` (stripping any frontmatter),
/// launches it as a hidden, short-lived pipeline on the target node, and
/// streams each served event to stdout as a compact JSON line. Diagnostics
/// emitted during execution are printed to stderr, color-coded by severity.
/// Streaming stops when the pipeline completes or on Ctrl-C.
pub async fn run(
    config: &ResolvedConfig,
    sources: TokenSources,
    _output: OutputMode,
    file: &Path,
) -> Result<(), HintedError> {
    let (workspace, node) = super::resolve_target(config)?;
    let desired = project::desired_from_file(file).map_err(HintedError::new)?;
    let client = super::platform_client(config, sources).await?;

    client
        .stream_run(
            &workspace,
            &node,
            &desired.definition,
            print_results,
            print_diagnostics,
        )
        .await
        .map_err(super::list::disconnected_hint)?;

    Ok(())
}

/// Print a page of result events to stdout as newline-delimited JSON.
fn print_results(events: &[serde_json::Value]) -> Result<(), Error> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for event in events {
        // Serve wraps each result as `{schema_id, data}`; unwrap `data` when
        // present, otherwise print the event verbatim.
        let value = event.get("data").unwrap_or(event);
        let line = serde_json::to_string(value).map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
        writeln!(out, "{line}").map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
    }
    out.flush().map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
    Ok(())
}

/// Print a page of diagnostics to stderr, color-coded by severity.
fn print_diagnostics(events: &[serde_json::Value]) -> Result<(), Error> {
    let stderr = std::io::stderr();
    let mut err = stderr.lock();
    for event in events {
        let value = event.get("data").unwrap_or(event);
        let Ok(diag) = serde_json::from_value::<Diagnostic>(value.clone()) else {
            continue;
        };
        let label = diag.severity.label();
        let text = diag.text();
        let line = match diag.severity {
            Severity::Error => format!("{}: {text}", label.red().bold()),
            Severity::Warning => format!("{}: {text}", label.yellow().bold()),
            _ => format!("{label}: {text}"),
        };
        writeln!(err, "{line}").map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
    }
    err.flush().map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
    Ok(())
}
