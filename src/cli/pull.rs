//! Handler for `tz project pull` — fetch platform pipelines into local files.
//!
//! The inverse of `tz project apply`: it reconciles the local project directory
//! to match the pipelines on the node. Remote pipelines with no local file are
//! written; those whose local file differs are overwritten; local files with no
//! matching pipeline are deleted (with `--prune`). Overwrites and deletes are
//! destructive and prompt for confirmation unless `--yes` is set.

use std::collections::BTreeSet;
use std::path::PathBuf;

use owo_colors::OwoColorize;
use serde::Serialize;

use crate::auth::TokenSources;
use crate::client::PlatformClient;
use crate::config::ResolvedConfig;
use crate::error::{Error, HintedError};
use crate::model::{DesiredState, RemotePipeline};
use crate::output::OutputMode;
use crate::project::{self, frontmatter::Frontmatter};

/// Exit code emitted when a dry-run finds pending changes (CI gating).
const EXIT_CHANGES_PENDING: u8 = 2;
/// Exit code for a partial failure while writing files.
const EXIT_PARTIAL_FAILURE: u8 = 3;

/// A single planned filesystem change.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Action {
    /// Write a new file for a remote pipeline with no local counterpart.
    Create {
        name: String,
        path: PathBuf,
        #[serde(skip)]
        content: String,
    },
    /// Overwrite a local file whose content differs from the platform.
    Overwrite {
        name: String,
        path: PathBuf,
        #[serde(skip)]
        content: String,
    },
    /// Delete a local file with no matching pipeline on the platform.
    Delete { name: String, path: PathBuf },
}

impl Action {
    /// The pipeline name this action concerns.
    fn name(&self) -> &str {
        match self {
            Action::Create { name, .. }
            | Action::Overwrite { name, .. }
            | Action::Delete { name, .. } => name,
        }
    }

    /// Whether this action mutates or removes an existing file.
    fn is_destructive(&self) -> bool {
        matches!(self, Action::Overwrite { .. } | Action::Delete { .. })
    }
}

/// The full set of changes `pull` would apply.
#[derive(Debug, Serialize)]
struct PullPlan {
    actions: Vec<Action>,
    /// Local files with no matching pipeline that are *not* deleted (no
    /// `--prune`), reported for visibility.
    orphans: Vec<OrphanFile>,
    /// The number of remote pipelines already in sync with local files.
    in_sync: usize,
    prune: bool,
}

/// A local file with no matching pipeline on the platform, left in place.
#[derive(Debug, Serialize)]
struct OrphanFile {
    name: String,
    path: PathBuf,
}

impl PullPlan {
    /// Whether the plan would change anything on disk.
    fn has_changes(&self) -> bool {
        !self.actions.is_empty()
    }
}

/// Handle `tz project pull`.
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
    let remote = client
        .list_pipelines(&workspace, &node)
        .await
        .map_err(super::list::disconnected_hint)?;
    let plan = compute_plan(config, &remote, prune)?;

    if dry_run {
        match output {
            OutputMode::Json => emit_json(&plan)?,
            OutputMode::Text => print!("{}", render(&plan)),
        }
        return Ok(if plan.has_changes() {
            EXIT_CHANGES_PENDING
        } else {
            0
        });
    }

    if !plan.has_changes() {
        if output == OutputMode::Text {
            println!("No changes. The project matches the platform.");
        } else {
            emit_json(&plan)?;
        }
        return Ok(0);
    }

    if output == OutputMode::Text {
        print!("{}", render(&plan));
    }

    let report = execute(&plan, yes)?;

    match output {
        OutputMode::Json => emit_json(&report)?,
        OutputMode::Text => report.print(),
    }

    Ok(if report.failed > 0 {
        EXIT_PARTIAL_FAILURE
    } else {
        0
    })
}

/// Build the plan by diffing remote pipelines against local project files.
fn compute_plan(
    config: &ResolvedConfig,
    remote: &[RemotePipeline],
    prune: bool,
) -> Result<PullPlan, HintedError> {
    let default_state = parse_default_state(config.default_state.as_deref());
    let local = project::load_project_with_paths(
        &config.project_root,
        &config.pipelines_glob,
        config.default_state.as_deref(),
    )
    .map_err(HintedError::new)?;

    // name -> (path, desired) for quick lookup.
    let local_by_name: std::collections::HashMap<&str, &(PathBuf, crate::model::DesiredPipeline)> =
        local
            .iter()
            .map(|entry| (entry.1.name.as_str(), entry))
            .collect();
    let remote_names: BTreeSet<&str> = remote.iter().map(|p| p.name.as_str()).collect();

    let base_dir = project::glob_base_dir(&config.project_root, &config.pipelines_glob);

    let mut actions = Vec::new();
    let mut in_sync = 0;

    for pipeline in remote {
        let effective_state = pipeline.state.to_desired().unwrap_or(default_state);
        match local_by_name.get(pipeline.name.as_str()) {
            Some((path, desired)) => {
                let content = render_pipeline(pipeline, path, default_state);
                // Compare semantically to avoid churn on formatting differences.
                if desired.definition == pipeline.definition.trim()
                    && desired.state == effective_state
                {
                    in_sync += 1;
                } else {
                    actions.push(Action::Overwrite {
                        name: pipeline.name.clone(),
                        path: path.clone(),
                        content,
                    });
                }
            }
            None => {
                let path = base_dir.join(format!("{}.tql", pipeline.name));
                let content = render_pipeline(pipeline, &path, default_state);
                actions.push(Action::Create {
                    name: pipeline.name.clone(),
                    path,
                    content,
                });
            }
        }
    }

    // Local files with no matching remote pipeline.
    let mut orphans = Vec::new();
    for (path, desired) in &local {
        if !remote_names.contains(desired.name.as_str()) {
            if prune {
                actions.push(Action::Delete {
                    name: desired.name.clone(),
                    path: path.clone(),
                });
            } else {
                orphans.push(OrphanFile {
                    name: desired.name.clone(),
                    path: path.clone(),
                });
            }
        }
    }

    // Deterministic ordering: creates, overwrites, deletes, each by name.
    actions.sort_by(|a, b| {
        action_rank(a)
            .cmp(&action_rank(b))
            .then(a.name().cmp(b.name()))
    });
    orphans.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(PullPlan {
        actions,
        orphans,
        in_sync,
        prune,
    })
}

/// A stable sort rank so plans list creates, then overwrites, then deletes.
fn action_rank(action: &Action) -> u8 {
    match action {
        Action::Create { .. } => 0,
        Action::Overwrite { .. } => 1,
        Action::Delete { .. } => 2,
    }
}

/// Render a remote pipeline as `.tql` file contents.
///
/// The `name` field is written only when it differs from the file stem, and the
/// `state` field only when the observed state maps to a declarable desired state
/// that differs from the project default (keeping files minimal and churn low).
fn render_pipeline(
    pipeline: &RemotePipeline,
    path: &std::path::Path,
    default_state: DesiredState,
) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str());
    let name = match stem {
        Some(stem) if stem == pipeline.name => None,
        _ => Some(pipeline.name.clone()),
    };
    let state = pipeline.state.to_desired().filter(|s| *s != default_state);
    let frontmatter = Frontmatter {
        name,
        description: None,
        state,
        node: None,
    };
    project::frontmatter::render(&frontmatter, &pipeline.definition)
}

/// Map the configured default-state string to a [`DesiredState`].
fn parse_default_state(s: Option<&str>) -> DesiredState {
    match s.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("paused") => DesiredState::Paused,
        Some("stopped") => DesiredState::Stopped,
        _ => DesiredState::Running,
    }
}

/// The outcome of applying a plan.
#[derive(Debug, Default, Serialize)]
struct Report {
    created: Vec<String>,
    overwritten: Vec<String>,
    deleted: Vec<String>,
    skipped: Vec<String>,
    failed: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    errors: Vec<String>,
}

impl Report {
    /// Render a human-readable summary.
    fn print(&self) {
        for name in &self.created {
            println!("{} created {name}", crate::symbols::OK.green().bold());
        }
        for name in &self.overwritten {
            println!("{} overwrote {name}", crate::symbols::OK.green().bold());
        }
        for name in &self.deleted {
            println!("{} deleted {name}", crate::symbols::OK.green().bold());
        }
        for name in &self.skipped {
            println!("{} skipped {name}", "-".yellow());
        }
        for err in &self.errors {
            println!("{} {err}", crate::symbols::ERR.red().bold());
        }
        println!(
            "Pulled: {} created, {} overwritten, {} deleted, {} skipped, {} failed.",
            self.created.len(),
            self.overwritten.len(),
            self.deleted.len(),
            self.skipped.len(),
            self.failed,
        );
    }
}

/// Apply the plan to disk, confirming each destructive action unless `--yes`.
fn execute(plan: &PullPlan, yes: bool) -> Result<Report, HintedError> {
    let mut report = Report::default();
    for action in &plan.actions {
        // Prompt before any destructive operation (overwrite or delete).
        if action.is_destructive() && !yes {
            let prompt = match action {
                Action::Overwrite { path, .. } => {
                    format!("{} Overwrite {}?", "!".red().bold(), path.display())
                }
                Action::Delete { path, .. } => {
                    format!("{} Delete {}?", "!".red().bold(), path.display())
                }
                Action::Create { .. } => unreachable!("create is not destructive"),
            };
            if !super::confirm(&prompt)? {
                report.skipped.push(action.name().to_string());
                continue;
            }
        }

        let result = match action {
            Action::Create { path, content, .. } | Action::Overwrite { path, content, .. } => {
                write_file(path, content)
            }
            Action::Delete { path, .. } => std::fs::remove_file(path)
                .map_err(|e| format!("cannot delete {}: {e}", path.display())),
        };

        match result {
            Ok(()) => match action {
                Action::Create { name, .. } => report.created.push(name.clone()),
                Action::Overwrite { name, .. } => report.overwritten.push(name.clone()),
                Action::Delete { name, .. } => report.deleted.push(name.clone()),
            },
            Err(e) => {
                report.failed += 1;
                report.errors.push(e);
            }
        }
    }
    Ok(report)
}

/// Write file contents, creating parent directories as needed.
fn write_file(path: &std::path::Path, content: &str) -> std::result::Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, content).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Render a plan as a stable, reviewable diff.
fn render(plan: &PullPlan) -> String {
    let mut out = String::new();
    for action in &plan.actions {
        let line = match action {
            Action::Create { name, path, .. } => format!(
                "{} {:<24} {:<10} {}",
                "+".green(),
                name,
                "create",
                path.display()
            ),
            Action::Overwrite { name, path, .. } => format!(
                "{} {:<24} {:<10} {}",
                "~".yellow(),
                name,
                "overwrite",
                path.display()
            ),
            Action::Delete { name, path, .. } => format!(
                "{} {:<24} {:<10} {}",
                "-".red(),
                name,
                "delete",
                path.display()
            ),
        };
        out.push_str(&line);
        out.push('\n');
    }
    for orphan in &plan.orphans {
        out.push_str(&format!(
            "{} {:<24} {:<10} {} (orphaned, needs --prune)\n",
            "-".red(),
            orphan.name,
            "delete",
            orphan.path.display()
        ));
    }

    if !plan.has_changes() && plan.orphans.is_empty() {
        out.push_str("No changes. The project matches the platform.\n");
        return out;
    }
    out.push_str(&summary(plan));
    out.push('\n');
    out
}

/// Render the trailing summary line.
fn summary(plan: &PullPlan) -> String {
    let creates = plan
        .actions
        .iter()
        .filter(|a| matches!(a, Action::Create { .. }))
        .count();
    let overwrites = plan
        .actions
        .iter()
        .filter(|a| matches!(a, Action::Overwrite { .. }))
        .count();
    let deletes = plan
        .actions
        .iter()
        .filter(|a| matches!(a, Action::Delete { .. }))
        .count();
    let mut parts = Vec::new();
    if creates > 0 {
        parts.push(format!("{creates} to create"));
    }
    if overwrites > 0 {
        parts.push(format!("{overwrites} to overwrite"));
    }
    if deletes > 0 {
        parts.push(format!("{deletes} to delete"));
    }
    if !plan.orphans.is_empty() {
        parts.push(format!("{} orphan", plan.orphans.len()));
    }
    if plan.in_sync > 0 {
        parts.push(format!("{} in sync", plan.in_sync));
    }
    if parts.is_empty() {
        parts.push("no changes".to_string());
    }
    format!("Plan: {}.", parts.join(", "))
}

/// Emit a JSON payload to stdout.
fn emit_json<T: Serialize>(value: &T) -> Result<(), HintedError> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| HintedError::new(Error::Other(e.into())))?;
    println!("{json}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LifecycleState, PipelineId};
    use std::path::Path;

    fn remote(name: &str, def: &str, state: LifecycleState) -> RemotePipeline {
        RemotePipeline {
            id: PipelineId(format!("id-{name}")),
            name: name.to_string(),
            definition: def.to_string(),
            state,
            error: None,
        }
    }

    fn config_for(root: &Path) -> ResolvedConfig {
        ResolvedConfig {
            api_endpoint: "http://localhost".to_string(),
            oidc_issuer: None,
            client_id: None,
            client_secret: None,
            oidc_audience: None,
            oidc_scope: None,
            workspace: None,
            node: None,
            pipelines_glob: "pipelines/**/*.tql".to_string(),
            default_state: None,
            id_token: None,
            token_file: None,
            config_dir: None,
            project_root: root.to_path_buf(),
        }
    }

    #[test]
    fn creates_new_files_for_remote_pipelines() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config_for(tmp.path());
        let remote = vec![remote("alpha", "version", LifecycleState::Running)];
        let plan = compute_plan(&config, &remote, false).unwrap();
        assert_eq!(plan.actions.len(), 1);
        assert!(matches!(&plan.actions[0], Action::Create { name, .. } if name == "alpha"));
        // A running pipeline with the default running state omits frontmatter.
        if let Action::Create { path, content, .. } = &plan.actions[0] {
            assert!(path.ends_with("pipelines/alpha.tql"));
            assert_eq!(content, "version\n");
        }
    }

    #[test]
    fn detects_in_sync_and_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("pipelines")).unwrap();
        std::fs::write(tmp.path().join("pipelines/alpha.tql"), "version\n").unwrap();
        std::fs::write(tmp.path().join("pipelines/beta.tql"), "old\n").unwrap();
        let config = config_for(tmp.path());
        let remote = vec![
            remote("alpha", "version", LifecycleState::Running),
            remote("beta", "new", LifecycleState::Running),
        ];
        let plan = compute_plan(&config, &remote, false).unwrap();
        assert_eq!(plan.in_sync, 1);
        assert_eq!(plan.actions.len(), 1);
        assert!(matches!(&plan.actions[0], Action::Overwrite { name, .. } if name == "beta"));
    }

    #[test]
    fn orphans_are_gated_by_prune() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("pipelines")).unwrap();
        std::fs::write(tmp.path().join("pipelines/gone.tql"), "version\n").unwrap();
        let config = config_for(tmp.path());
        let remote: Vec<RemotePipeline> = vec![];

        // Without prune: reported as an orphan, no action.
        let plan = compute_plan(&config, &remote, false).unwrap();
        assert!(plan.actions.is_empty());
        assert_eq!(plan.orphans.len(), 1);

        // With prune: a delete action.
        let plan = compute_plan(&config, &remote, true).unwrap();
        assert_eq!(plan.actions.len(), 1);
        assert!(matches!(&plan.actions[0], Action::Delete { name, .. } if name == "gone"));
    }

    #[test]
    fn paused_pipeline_writes_state_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config_for(tmp.path());
        let remote = vec![remote("p", "version", LifecycleState::Paused)];
        let plan = compute_plan(&config, &remote, false).unwrap();
        if let Action::Create { content, .. } = &plan.actions[0] {
            assert!(content.contains("state: paused"));
        } else {
            panic!("expected create");
        }
    }

    #[test]
    fn execute_creates_files() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config_for(tmp.path());
        let remote = vec![remote("alpha", "version", LifecycleState::Running)];
        let plan = compute_plan(&config, &remote, false).unwrap();
        let report = execute(&plan, true).unwrap();
        assert_eq!(report.created, vec!["alpha"]);
        let written = std::fs::read_to_string(tmp.path().join("pipelines/alpha.tql")).unwrap();
        assert_eq!(written, "version\n");
    }
}
