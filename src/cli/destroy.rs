//! Handler for `tz project destroy` — remove the project's pipelines from the node.

use std::collections::BTreeSet;

use owo_colors::OwoColorize;

use crate::apply::{self, ApplyReport};
use crate::auth::TokenSources;
use crate::client::PlatformClient;
use crate::config::ResolvedConfig;
use crate::error::{Error, HintedError};
use crate::output::OutputMode;
use crate::project;
use crate::reconcile::{Action, Plan};

/// Exit code for a partial failure during destroy.
const EXIT_PARTIAL_FAILURE: u8 = 3;

/// Handle `tz project destroy`.
///
/// Deletes every pipeline whose name is declared by the local project and that
/// currently exists on the node. Always confirms unless `--yes`.
pub async fn run(
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
    yes: bool,
) -> Result<u8, HintedError> {
    let (workspace, node) = super::resolve_target(config)?;

    let desired = project::load_project(
        &config.project_root,
        &config.pipelines_glob,
        config.default_state.as_deref(),
    )
    .map_err(HintedError::new)?;
    let names: BTreeSet<&str> = desired.iter().map(|p| p.name.as_str()).collect();

    let client = super::platform_client(config, sources).await?;
    let actual = client
        .list_pipelines(&workspace, &node)
        .await
        .map_err(super::list::disconnected_hint)?;

    // A delete-only plan for project pipelines that exist on the node.
    let actions: Vec<Action> = actual
        .iter()
        .filter(|p| names.contains(p.name.as_str()))
        .map(|p| Action::Delete {
            remote: p.id.clone(),
            name: p.name.clone(),
        })
        .collect();

    if actions.is_empty() {
        if output == OutputMode::Text {
            println!("Nothing to destroy: no project pipelines exist on the node.");
        } else {
            emit_json(&ApplyReport::default())?;
        }
        return Ok(0);
    }

    let del_names: Vec<&str> = actions.iter().map(|a| a.name()).collect();
    if output == OutputMode::Text {
        println!(
            "{} About to DELETE {} project pipeline(s) from the node:",
            "!".red().bold(),
            actions.len()
        );
        for name in &del_names {
            println!("  - {name}");
        }
    }

    if !yes {
        let prompt = format!("Permanently delete these {} pipeline(s)?", actions.len());
        if !super::confirm(&prompt)? {
            if output == OutputMode::Text {
                println!("Aborted.");
            }
            return Ok(0);
        }
    }

    let plan = Plan {
        actions,
        orphans: Vec::new(),
        terminal: Vec::new(),
        prune: false,
    };
    let report = apply::execute(&client, &workspace, &node, &plan).await;

    match output {
        OutputMode::Json => emit_json(&report)?,
        OutputMode::Text => {
            for ok in &report.succeeded {
                println!("{} deleted {}", crate::symbols::OK.green().bold(), ok.name);
            }
            for fail in &report.failed {
                println!(
                    "{} {}: {}",
                    crate::symbols::ERR.red().bold(),
                    fail.name,
                    fail.error.as_deref().unwrap_or("unknown error")
                );
            }
            println!(
                "Destroyed: {} succeeded, {} failed.",
                report.succeeded.len(),
                report.failed.len()
            );
        }
    }

    Ok(if report.has_failures() {
        EXIT_PARTIAL_FAILURE
    } else {
        0
    })
}

/// Emit a JSON payload to stdout.
fn emit_json<T: serde::Serialize>(value: &T) -> Result<(), HintedError> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| HintedError::new(Error::Other(e.into())))?;
    println!("{json}");
    Ok(())
}
