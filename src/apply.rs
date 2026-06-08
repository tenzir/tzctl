//! Plan executor: applies a [`Plan`] against a [`PlatformClient`].
//!
//! Actions run in the reconciler's deterministic order (deletes → creates →
//! sets → transitions). Each action is isolated: a failure is recorded and the
//! run continues, so one bad pipeline never aborts the rest. Re-running on a
//! converged node is a no-op because the reconciler produces an empty plan.

use serde::Serialize;

use crate::client::PlatformClient;
use crate::error::Error;
use crate::model::{NodeId, TenantId};
use crate::reconcile::{Action, Plan};

/// A single applied action's outcome, for reporting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActionOutcome {
    /// The action kind (`create`/`set`/`delete`/`transition`).
    pub kind: String,
    /// The pipeline name.
    pub name: String,
    /// An error message when the action failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The aggregated result of executing a plan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ApplyReport {
    /// Actions that succeeded.
    pub succeeded: Vec<ActionOutcome>,
    /// Actions that failed, each with its error.
    pub failed: Vec<ActionOutcome>,
}

impl ApplyReport {
    /// Whether any action failed (drives the `apply` exit code `3`).
    pub fn has_failures(&self) -> bool {
        !self.failed.is_empty()
    }

    /// Total number of actions attempted.
    #[allow(dead_code)] // used by tests; handy for reporting in stage 8.
    pub fn attempted(&self) -> usize {
        self.succeeded.len() + self.failed.len()
    }
}

/// Execute `plan` against `client`, returning a per-action report.
///
/// Never aborts on a single failure; all actions are attempted.
pub async fn execute<C: PlatformClient>(
    client: &C,
    workspace: &TenantId,
    node: &NodeId,
    plan: &Plan,
) -> ApplyReport {
    let mut report = ApplyReport::default();
    for action in &plan.actions {
        let result = apply_one(client, workspace, node, action).await;
        let outcome = ActionOutcome {
            kind: action.kind().to_string(),
            name: action.name().to_string(),
            error: result.err().map(|e| e.to_string()),
        };
        if outcome.error.is_some() {
            report.failed.push(outcome);
        } else {
            report.succeeded.push(outcome);
        }
    }
    report
}

/// Apply a single action via the matching client method.
async fn apply_one<C: PlatformClient>(
    client: &C,
    workspace: &TenantId,
    node: &NodeId,
    action: &Action,
) -> Result<(), Error> {
    match action {
        Action::Create(pipeline) => {
            // `create` also drives the pipeline to its desired state.
            client.create(workspace, node, pipeline).await?;
            Ok(())
        }
        Action::Set { remote, to } => client.set(workspace, node, remote, to).await,
        Action::Delete { remote, .. } => client.delete(workspace, node, remote).await,
        Action::Transition { remote, action, .. } => {
            client.transition(workspace, node, remote, *action).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::MockClient;
    use crate::model::{DesiredPipeline, DesiredState, LifecycleState, PipelineId, RemotePipeline};
    use crate::reconcile::{ReconcileOpts, reconcile};

    fn desired(name: &str, def: &str, state: DesiredState) -> DesiredPipeline {
        DesiredPipeline {
            name: name.to_string(),
            definition: def.to_string(),
            state,
            node: None,
        }
    }

    fn remote(id: &str, name: &str, def: &str, state: LifecycleState) -> RemotePipeline {
        RemotePipeline {
            id: PipelineId(id.to_string()),
            name: name.to_string(),
            definition: def.to_string(),
            state,
            error: None,
        }
    }

    fn tenant() -> TenantId {
        TenantId("t-abcd1234".to_string())
    }
    fn node() -> NodeId {
        NodeId("n-w2tjezz3".to_string())
    }

    /// A mock that records every node-proxy endpoint it is asked to serve.
    fn ok_mock() -> MockClient {
        let mut proxy = std::collections::HashMap::new();
        for ep in ["pipeline/create", "pipeline/update", "pipeline/delete"] {
            proxy.insert(ep.to_string(), "{}".to_string());
        }
        // create returns an id.
        proxy.insert(
            "pipeline/create".to_string(),
            r#"{"id":"new-id"}"#.to_string(),
        );
        MockClient {
            proxy_responses: proxy,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn empty_plan_is_noop() {
        let plan = reconcile(&[], &[], ReconcileOpts::default());
        let report = execute(&ok_mock(), &tenant(), &node(), &plan).await;
        assert_eq!(report.attempted(), 0);
        assert!(!report.has_failures());
    }

    #[tokio::test]
    async fn applies_create_set_transition() {
        let desired = vec![
            desired("new", "version", DesiredState::Running), // create
            desired("drift", "v2", DesiredState::Running),    // set
            desired("paused-me", "version", DesiredState::Paused), // transition
        ];
        let actual = vec![
            remote("id-drift", "drift", "v1", LifecycleState::Running),
            remote("id-pause", "paused-me", "version", LifecycleState::Running),
        ];
        let plan = reconcile(&desired, &actual, ReconcileOpts::default());
        let report = execute(&ok_mock(), &tenant(), &node(), &plan).await;
        assert_eq!(report.succeeded.len(), 3);
        assert!(!report.has_failures());
    }

    #[tokio::test]
    async fn prune_deletes_orphan() {
        let actual = vec![remote("id-old", "old", "version", LifecycleState::Running)];
        let plan = reconcile(&[], &actual, ReconcileOpts { prune: true });
        let report = execute(&ok_mock(), &tenant(), &node(), &plan).await;
        assert_eq!(report.succeeded.len(), 1);
        assert_eq!(report.succeeded[0].kind, "delete");
    }

    #[tokio::test]
    async fn failure_is_isolated_and_others_continue() {
        // The mock disconnects on `pipeline/update`, failing the Set, but the
        // Create still succeeds.
        let mut proxy = std::collections::HashMap::new();
        proxy.insert(
            "pipeline/create".to_string(),
            r#"{"id":"new-id"}"#.to_string(),
        );
        let mut disconnected = std::collections::HashSet::new();
        disconnected.insert("pipeline/update".to_string());
        let mock = MockClient {
            proxy_responses: proxy,
            disconnected_endpoints: disconnected,
            ..Default::default()
        };

        let desired = vec![
            desired("new", "version", DesiredState::Stopped), // create, no transition
            desired("drift", "v2", DesiredState::Running),    // set -> fails
        ];
        let actual = vec![remote("id-drift", "drift", "v1", LifecycleState::Running)];
        let plan = reconcile(&desired, &actual, ReconcileOpts::default());
        let report = execute(&mock, &tenant(), &node(), &plan).await;

        assert!(report.has_failures());
        assert_eq!(report.succeeded.len(), 1);
        assert_eq!(report.succeeded[0].kind, "create");
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].kind, "set");
        assert!(report.failed[0].error.is_some());
    }
}
