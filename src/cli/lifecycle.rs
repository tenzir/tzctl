//! Handlers for the single-pipeline mutating commands: `create`, `set`,
//! `delete`, `pause`, `unpause`, and `stop`.

use std::path::Path;

use owo_colors::OwoColorize;
use serde::Serialize;

use crate::auth::TokenSources;
use crate::client::PlatformClient;
use crate::config::ResolvedConfig;
use crate::error::{Error, HintedError};
use crate::model::{LifecycleState, RemotePipeline, TransitionAction};
use crate::output::OutputMode;
use crate::project;

/// The machine-readable result of a mutating command.
#[derive(Debug, Serialize)]
struct ActionResult {
    /// The action performed (`create`, `set`, `delete`, `start`, …).
    action: String,
    /// The pipeline name.
    name: String,
    /// The pipeline id, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    /// The resulting (or intended) state, when meaningful.
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    /// A human-readable note (e.g. for no-ops).
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

/// Emit an [`ActionResult`] as JSON (text output is printed by the handlers).
fn emit(output: OutputMode, result: &ActionResult) -> Result<(), HintedError> {
    if output == OutputMode::Json {
        let json = serde_json::to_string_pretty(result)
            .map_err(|e| HintedError::new(Error::Other(e.into())))?;
        println!("{json}");
    }
    Ok(())
}

/// Map a node-disconnected error to a friendly hint (shared with `list`).
fn map_err(error: Error) -> HintedError {
    super::list::disconnected_hint(error)
}

/// Handle `tz pipeline create <file>`.
pub async fn create(
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
    file: &Path,
) -> Result<(), HintedError> {
    let (workspace, node) = super::resolve_target(config)?;
    let desired = project::desired_from_file(file).map_err(HintedError::new)?;
    let client = super::platform_client(config, sources).await?;

    // `create` also drives the pipeline to its desired state.
    let id = client
        .create(&workspace, &node, &desired)
        .await
        .map_err(map_err)?;

    print_text(
        output,
        format!(
            "{} Created {} ({}) {} {:?}",
            crate::symbols::OK.green().bold(),
            desired.name.bold(),
            id,
            crate::symbols::TRANSITION,
            desired.state
        ),
    );
    emit(
        output,
        &ActionResult {
            action: "create".to_string(),
            name: desired.name,
            id: Some(id.to_string()),
            state: Some(format!("{:?}", desired.state).to_lowercase()),
            note: None,
        },
    )
}

/// Handle `tz pipeline set <file>` — in-place definition/name update.
pub async fn set(
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
    file: &Path,
) -> Result<(), HintedError> {
    let (workspace, node) = super::resolve_target(config)?;
    let desired = project::desired_from_file(file).map_err(HintedError::new)?;
    let client = super::platform_client(config, sources).await?;

    let remote = client
        .resolve_pipeline(&workspace, &node, &desired.name)
        .await
        .map_err(map_err)?;
    client
        .set(&workspace, &node, &remote.id, &desired)
        .await
        .map_err(map_err)?;

    print_text(
        output,
        format!(
            "{} Updated {} ({}) in place",
            crate::symbols::OK.green().bold(),
            desired.name.bold(),
            remote.id
        ),
    );
    emit(
        output,
        &ActionResult {
            action: "set".to_string(),
            name: desired.name,
            id: Some(remote.id.to_string()),
            state: None,
            note: None,
        },
    )
}

/// Handle `tz pipeline delete <name>` — requires confirmation unless `--yes`.
pub async fn delete(
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
    name: &str,
    yes: bool,
) -> Result<(), HintedError> {
    let (workspace, node) = super::resolve_target(config)?;
    let client = super::platform_client(config, sources).await?;
    let remote = client
        .resolve_pipeline(&workspace, &node, name)
        .await
        .map_err(map_err)?;

    if !yes && !super::confirm(&format!("Delete pipeline {name:?} ({})?", remote.id))? {
        print_text(output, "Aborted.".to_string());
        return emit(
            output,
            &ActionResult {
                action: "delete".to_string(),
                name: name.to_string(),
                id: Some(remote.id.to_string()),
                state: None,
                note: Some("aborted".to_string()),
            },
        );
    }

    client
        .delete(&workspace, &node, &remote.id)
        .await
        .map_err(map_err)?;
    print_text(
        output,
        format!(
            "{} Deleted {}",
            crate::symbols::OK.green().bold(),
            name.bold()
        ),
    );
    emit(
        output,
        &ActionResult {
            action: "delete".to_string(),
            name: name.to_string(),
            id: Some(remote.id.to_string()),
            state: None,
            note: None,
        },
    )
}

/// Handle `tz pipeline start <name>`.
pub async fn start(
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
    name: &str,
) -> Result<(), HintedError> {
    transition_command(
        config,
        sources,
        output,
        name,
        TransitionAction::Start,
        "running",
        |s| matches!(s, LifecycleState::Running),
    )
    .await
}

/// Handle `tz pipeline stop <name>`.
pub async fn stop(
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
    name: &str,
) -> Result<(), HintedError> {
    transition_command(
        config,
        sources,
        output,
        name,
        TransitionAction::Stop,
        "stopped",
        |s| matches!(s, LifecycleState::Stopped | LifecycleState::Completed),
    )
    .await
}

/// Shared implementation for the run-state transition commands.
///
/// `already` reports whether the observed state already satisfies the target,
/// in which case the command is a no-op with a friendly note.
async fn transition_command(
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
    name: &str,
    action: TransitionAction,
    target_label: &str,
    already: impl Fn(&LifecycleState) -> bool,
) -> Result<(), HintedError> {
    let (workspace, node) = super::resolve_target(config)?;
    let client = super::platform_client(config, sources).await?;
    let remote: RemotePipeline = client
        .resolve_pipeline(&workspace, &node, name)
        .await
        .map_err(map_err)?;

    if already(&remote.state) {
        let note = format!("already {target_label}");
        print_text(
            output,
            format!(
                "{} {} is {note}",
                crate::symbols::BULLET.dimmed(),
                name.bold()
            ),
        );
        return emit(
            output,
            &ActionResult {
                action: action.as_wire().to_string(),
                name: name.to_string(),
                id: Some(remote.id.to_string()),
                state: Some(target_label.to_string()),
                note: Some(note),
            },
        );
    }

    client
        .transition(&workspace, &node, &remote.id, action)
        .await
        .map_err(map_err)?;
    print_text(
        output,
        format!(
            "{} {} {} {target_label}",
            crate::symbols::OK.green().bold(),
            name.bold(),
            crate::symbols::TRANSITION
        ),
    );
    emit(
        output,
        &ActionResult {
            action: action.as_wire().to_string(),
            name: name.to_string(),
            id: Some(remote.id.to_string()),
            state: Some(target_label.to_string()),
            note: None,
        },
    )
}

/// Print a line only in text mode (JSON mode emits via [`emit`]).
fn print_text(output: OutputMode, line: String) {
    if output == OutputMode::Text {
        println!("{line}");
    }
}
