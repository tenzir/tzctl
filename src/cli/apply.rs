//! Handler for `tz project apply` — reconcile the node to the project.

use owo_colors::OwoColorize;
use serde::Serialize;

use crate::apply::{self, ApplyReport};
use crate::auth::TokenSources;
use crate::config::ResolvedConfig;
use crate::error::{Error, HintedError};
use crate::output::OutputMode;
use crate::reconcile::Plan;

/// Exit code for a partial failure during apply.
const EXIT_PARTIAL_FAILURE: u8 = 3;

/// The combined JSON payload for `tz project apply --output json`.
#[derive(Serialize)]
struct ApplyJson<'a> {
    plan: &'a Plan,
    report: &'a ApplyReport,
}

/// Handle `tz project apply`.
pub async fn run(
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
    prune: bool,
    dry_run: bool,
    yes: bool,
) -> Result<u8, HintedError> {
    let (workspace, node) = super::resolve_target(config)?;
    let client = super::platform_client(config, sources).await?;
    let plan = super::plan::compute(config, &client, &workspace, &node, prune).await?;

    // `--dry-run` behaves like `tz project plan`.
    if dry_run {
        match output {
            OutputMode::Json => emit_json(&ApplyJson {
                plan: &plan,
                report: &ApplyReport::default(),
            })?,
            OutputMode::Text => print!("{}", super::plan::render(&plan)),
        }
        return Ok(if plan.has_changes() {
            super::plan::EXIT_CHANGES_PENDING
        } else {
            0
        });
    }

    if !plan.has_changes() {
        if output == OutputMode::Text {
            println!("No changes. The node matches the project.");
        } else {
            emit_json(&ApplyJson {
                plan: &plan,
                report: &ApplyReport::default(),
            })?;
        }
        return Ok(0);
    }

    // Show the plan first.
    if output == OutputMode::Text {
        print!("{}", super::plan::render(&plan));
    }

    // Confirm destructive actions (deletes) unless --yes.
    let counts = plan.counts();
    if counts.delete > 0 && !yes {
        let names: Vec<&str> = plan
            .actions
            .iter()
            .filter(|a| a.kind() == "delete")
            .map(|a| a.name())
            .collect();
        let prompt = format!(
            "{} This will DELETE {} pipeline(s) not in the project: {}. Continue?",
            "!".red().bold(),
            counts.delete,
            names.join(", ")
        );
        if !super::confirm(&prompt)? {
            if output == OutputMode::Text {
                println!("Aborted.");
            }
            return Ok(0);
        }
    }

    let report = apply::execute(&client, &workspace, &node, &plan).await;

    match output {
        OutputMode::Json => emit_json(&ApplyJson {
            plan: &plan,
            report: &report,
        })?,
        OutputMode::Text => print_report(&report),
    }

    Ok(if report.has_failures() {
        EXIT_PARTIAL_FAILURE
    } else {
        0
    })
}

/// Render an apply report as human-readable text.
fn print_report(report: &ApplyReport) {
    for ok in &report.succeeded {
        println!("{} {} {}", "✓".green().bold(), ok.kind, ok.name);
    }
    for fail in &report.failed {
        println!(
            "{} {} {}: {}",
            "✗".red().bold(),
            fail.kind,
            fail.name,
            fail.error.as_deref().unwrap_or("unknown error")
        );
    }
    println!(
        "Applied: {} succeeded, {} failed.",
        report.succeeded.len(),
        report.failed.len()
    );
}

/// Emit a JSON payload to stdout.
fn emit_json<T: Serialize>(value: &T) -> Result<(), HintedError> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| HintedError::new(Error::Other(e.into())))?;
    println!("{json}");
    Ok(())
}
