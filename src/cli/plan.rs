//! Handler for `tz project plan` — the read-only desired-vs-actual diff.

use owo_colors::OwoColorize;

use crate::auth::TokenSources;
use crate::client::PlatformApi;
use crate::client::PlatformClient;
use crate::config::ResolvedConfig;
use crate::error::{Error, HintedError};
use crate::model::{NodeId, TenantId};
use crate::output::OutputMode;
use crate::project;
use crate::reconcile::{self, Action, Plan, ReconcileOpts};

/// Exit code emitted when the plan contains pending changes (CI gating).
pub(super) const EXIT_CHANGES_PENDING: u8 = 2;

/// Load the project and the node's actual state, then reconcile into a plan.
pub(super) async fn compute(
    config: &ResolvedConfig,
    client: &PlatformApi,
    workspace: &TenantId,
    node: &NodeId,
    prune: bool,
) -> Result<Plan, HintedError> {
    let desired = project::load_project(
        &config.project_root,
        &config.pipelines_glob,
        config.default_state.as_deref(),
    )
    .map_err(HintedError::new)?;
    let actual = client
        .list_pipelines(workspace, node)
        .await
        .map_err(super::list::disconnected_hint)?;
    Ok(reconcile::reconcile(
        &desired,
        &actual,
        ReconcileOpts { prune },
    ))
}

/// Handle `tz project plan`.
///
/// Read-only. Returns exit code `0` when in sync, `2` when changes are pending.
pub async fn run(
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
    prune: bool,
) -> Result<u8, HintedError> {
    let (workspace, node) = super::resolve_target(config)?;
    let client = super::platform_client(config, sources).await?;
    let plan = compute(config, &client, &workspace, &node, prune).await?;

    match output {
        OutputMode::Json => {
            let json = serde_json::to_string_pretty(&plan)
                .map_err(|e| HintedError::new(Error::Other(e.into())))?;
            println!("{json}");
        }
        OutputMode::Text => print!("{}", render(&plan)),
    }

    Ok(if plan.has_changes() {
        EXIT_CHANGES_PENDING
    } else {
        0
    })
}

/// Render a plan as a stable, reviewable diff.
pub(super) fn render(plan: &Plan) -> String {
    let mut out = String::new();
    for action in &plan.actions {
        out.push_str(&render_action(action));
        out.push('\n');
    }
    // Orphans that would not be deleted (no --prune) are shown as gated.
    if !plan.prune {
        for orphan in &plan.orphans {
            out.push_str(&format!(
                "{} {:<24} {:<10} (orphaned, needs --prune)\n",
                "-".red(),
                orphan.name,
                "delete"
            ));
        }
    }
    for term in &plan.terminal {
        out.push_str(&format!(
            "{} {:<24} {:<10} ({}, not forced)\n",
            "!".yellow(),
            term.name,
            "terminal",
            term.state
        ));
    }

    if !plan.has_changes() && plan.terminal.is_empty() {
        out.push_str("No changes. The node matches the project.\n");
        return out;
    }
    out.push_str(&summary(plan));
    out.push('\n');
    out
}

/// Render a single action line.
fn render_action(action: &Action) -> String {
    match action {
        Action::Create(p) => format!(
            "{} {:<24} {:<10} -> {}",
            "+".green(),
            p.name,
            "create",
            format!("{:?}", p.state).to_lowercase()
        ),
        Action::Set { to, .. } => format!(
            "{} {:<24} {:<10} definition changed",
            "~".yellow(),
            to.name,
            "set"
        ),
        Action::Delete { name, .. } => {
            format!("{} {:<24} {:<10} (--prune)", "-".red(), name, "delete")
        }
        Action::Transition { name, from, to, .. } => format!(
            "{} {:<24} {:<10} {} -> {}",
            "⏸".yellow(),
            name,
            "transition",
            from,
            format!("{to:?}").to_lowercase()
        ),
    }
}

/// Render the trailing summary line.
fn summary(plan: &Plan) -> String {
    let c = plan.counts();
    let mut parts = Vec::new();
    if c.create > 0 {
        parts.push(format!("{} to create", c.create));
    }
    if c.set > 0 {
        parts.push(format!("{} to set", c.set));
    }
    if c.transition > 0 {
        parts.push(format!("{} state change", c.transition));
    }
    if c.delete > 0 {
        parts.push(format!("{} to delete", c.delete));
    }
    if c.orphans > 0 && !plan.prune {
        parts.push(format!("{} orphan", c.orphans));
    }
    if parts.is_empty() {
        parts.push("no changes".to_string());
    }
    format!("Plan: {}.", parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{MockClient, PlatformClient};
    use crate::model::{NodeId, TenantId};
    use std::path::Path;

    /// Build actual state from the pinned `pipeline_list.json` fixture.
    async fn fixture_actual() -> Vec<crate::model::RemotePipeline> {
        let mut proxy = std::collections::HashMap::new();
        proxy.insert(
            "pipeline/list".to_string(),
            include_str!("../../tests/fixtures/pipeline_list.json").to_string(),
        );
        let mock = MockClient {
            proxy_responses: proxy,
            ..Default::default()
        };
        mock.list_pipelines(
            &TenantId("t-abcd1234".to_string()),
            &NodeId("n-w2tjezz3".to_string()),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn plan_against_fixture_project_and_mock() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project");
        let desired = project::load_project(&root, "pipelines/**/*.tql", None).unwrap();
        let actual = fixture_actual().await;

        // Without prune: zeek-import is created; wrong-pipeline is a gated orphan;
        // user-assigned-name is in sync.
        let plan = reconcile::reconcile(&desired, &actual, ReconcileOpts { prune: false });
        let text = render(&plan);
        assert!(text.contains("zeek-import"));
        assert!(text.contains("create"));
        assert!(text.contains("wrong-pipeline"));
        assert!(text.contains("needs --prune"));
        assert!(plan.has_changes());
        assert_eq!(plan.counts().create, 1);
        assert_eq!(plan.counts().orphans, 1);

        // JSON serializes the plan.
        let json = serde_json::to_string(&plan).unwrap();
        assert!(json.contains("\"kind\":\"create\""));

        // With prune the orphan becomes a delete action.
        let pruned = reconcile::reconcile(&desired, &actual, ReconcileOpts { prune: true });
        assert_eq!(pruned.counts().delete, 1);
    }

    #[test]
    fn render_reports_in_sync() {
        let plan = Plan {
            actions: vec![],
            orphans: vec![],
            terminal: vec![],
            prune: false,
        };
        assert!(render(&plan).contains("No changes"));
        assert!(!plan.has_changes());
    }
}
