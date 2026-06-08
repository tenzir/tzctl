//! Pure desired-vs-actual diff engine.
//!
//! [`reconcile`] takes the desired pipelines (from the project) and the actual
//! pipelines (from the node) and returns a [`Plan`]. It performs no I/O, so it
//! is exhaustively unit-testable.
//!
//! Identity is by **pipeline name** (no labels in the MVP). Drift is detected
//! by comparing trimmed definitions; `name` is the join key and `state` is
//! handled via transitions. There is no `description` on the node API (stage
//! 4), so it never participates in drift.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::model::{
    DesiredPipeline, DesiredState, LifecycleState, PipelineId, RemotePipeline, TransitionAction,
    transition_for,
};

/// Options controlling reconciliation.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReconcileOpts {
    /// When set, actual-only ("orphan") pipelines are scheduled for deletion.
    pub prune: bool,
}

/// A single executable action in a [`Plan`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Action {
    /// Create a new pipeline (the client drives it to its desired state).
    Create(DesiredPipeline),
    /// Update a pipeline's definition in place.
    Set {
        /// The node-assigned id of the pipeline to update.
        remote: PipelineId,
        /// The desired pipeline to apply.
        to: DesiredPipeline,
    },
    /// Delete a pipeline (only emitted with `--prune`).
    Delete {
        /// The id of the pipeline to delete.
        remote: PipelineId,
        /// The pipeline name (for display).
        name: String,
    },
    /// Change a pipeline's run-state.
    Transition {
        /// The id of the pipeline to transition.
        remote: PipelineId,
        /// The pipeline name (for display).
        name: String,
        /// The observed state.
        from: LifecycleState,
        /// The desired state.
        to: DesiredState,
        /// The action sent to the node.
        action: TransitionAction,
    },
}

impl Action {
    /// The action kind as a stable lowercase label.
    pub fn kind(&self) -> &'static str {
        match self {
            Action::Create(_) => "create",
            Action::Set { .. } => "set",
            Action::Delete { .. } => "delete",
            Action::Transition { .. } => "transition",
        }
    }

    /// The pipeline name this action targets.
    pub fn name(&self) -> &str {
        match self {
            Action::Create(p) => &p.name,
            Action::Set { to, .. } => &to.name,
            Action::Delete { name, .. } => name,
            Action::Transition { name, .. } => name,
        }
    }
}

/// An actual-only pipeline that has no desired counterpart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Orphan {
    /// The node-assigned id.
    pub remote: PipelineId,
    /// The pipeline name.
    pub name: String,
}

/// A pipeline in a terminal state, reported but never force-transitioned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Terminal {
    /// The pipeline name.
    pub name: String,
    /// The observed terminal state.
    pub state: LifecycleState,
    /// The desired state (for context).
    pub desired: DesiredState,
}

/// The computed reconciliation plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    /// Ordered, executable actions.
    pub actions: Vec<Action>,
    /// Actual-only pipelines (deleted iff `--prune`).
    pub orphans: Vec<Orphan>,
    /// Pipelines in terminal states, reported only.
    pub terminal: Vec<Terminal>,
    /// Whether pruning was requested.
    pub prune: bool,
}

impl Plan {
    /// Whether the plan implies changes (drives the `plan` exit code).
    ///
    /// Orphans count as pending changes even without `--prune`, since they
    /// represent state the project does not declare.
    pub fn has_changes(&self) -> bool {
        !self.actions.is_empty() || !self.orphans.is_empty()
    }

    /// Count of each action kind, plus orphans, for the summary line.
    pub fn counts(&self) -> PlanCounts {
        let mut c = PlanCounts {
            orphans: self.orphans.len(),
            ..Default::default()
        };
        for action in &self.actions {
            match action {
                Action::Create(_) => c.create += 1,
                Action::Set { .. } => c.set += 1,
                Action::Delete { .. } => c.delete += 1,
                Action::Transition { .. } => c.transition += 1,
            }
        }
        c
    }
}

/// Per-kind action counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PlanCounts {
    /// Pipelines to create.
    pub create: usize,
    /// Pipelines to update in place.
    pub set: usize,
    /// Pipelines to delete.
    pub delete: usize,
    /// Run-state transitions.
    pub transition: usize,
    /// Actual-only orphans.
    pub orphans: usize,
}

/// Whether a state is terminal (and should not be force-transitioned).
fn is_terminal(state: &LifecycleState) -> bool {
    matches!(state, LifecycleState::Completed | LifecycleState::Failed)
}

/// Whether a desired pipeline has drifted from its actual counterpart.
///
/// Drift is a difference in the (whitespace-trimmed) `definition`. `name` is
/// the join key; `state` is reconciled via transitions, not drift.
fn drifted(desired: &DesiredPipeline, actual: &RemotePipeline) -> bool {
    desired.definition.trim() != actual.definition.trim()
}

/// Compute the plan to make `actual` match `desired`.
///
/// Actions are ordered deterministically: deletes → creates → sets →
/// transitions, so a rename (delete old + create new) never collides.
pub fn reconcile(
    desired: &[DesiredPipeline],
    actual: &[RemotePipeline],
    opts: ReconcileOpts,
) -> Plan {
    let actual_by_name: BTreeMap<&str, &RemotePipeline> =
        actual.iter().map(|p| (p.name.as_str(), p)).collect();
    let desired_by_name: BTreeMap<&str, &DesiredPipeline> =
        desired.iter().map(|p| (p.name.as_str(), p)).collect();

    let mut deletes = Vec::new();
    let mut creates = Vec::new();
    let mut sets = Vec::new();
    let mut transitions = Vec::new();
    let mut orphans = Vec::new();
    let mut terminal = Vec::new();

    // Desired pipelines: create, set (drift), and/or transition.
    for d in desired {
        match actual_by_name.get(d.name.as_str()) {
            None => creates.push(Action::Create(d.clone())),
            Some(a) => {
                if drifted(d, a) {
                    sets.push(Action::Set {
                        remote: a.id.clone(),
                        to: d.clone(),
                    });
                }
                if is_terminal(&a.state) {
                    terminal.push(Terminal {
                        name: d.name.clone(),
                        state: a.state.clone(),
                        desired: d.state,
                    });
                } else if let Some(action) = transition_for(&a.state, d.state) {
                    transitions.push(Action::Transition {
                        remote: a.id.clone(),
                        name: d.name.clone(),
                        from: a.state.clone(),
                        to: d.state,
                        action,
                    });
                }
            }
        }
    }

    // Actual-only pipelines are orphans; deleted only with --prune.
    for a in actual {
        if !desired_by_name.contains_key(a.name.as_str()) {
            orphans.push(Orphan {
                remote: a.id.clone(),
                name: a.name.clone(),
            });
            if opts.prune {
                deletes.push(Action::Delete {
                    remote: a.id.clone(),
                    name: a.name.clone(),
                });
            }
        }
    }

    let mut actions = Vec::new();
    actions.extend(deletes);
    actions.extend(creates);
    actions.extend(sets);
    actions.extend(transitions);

    Plan {
        actions,
        orphans,
        terminal,
        prune: opts.prune,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn create_when_desired_only() {
        let plan = reconcile(
            &[desired("a", "version", DesiredState::Running)],
            &[],
            ReconcileOpts::default(),
        );
        assert_eq!(plan.actions.len(), 1);
        assert!(matches!(plan.actions[0], Action::Create(_)));
        assert!(plan.has_changes());
    }

    #[test]
    fn no_op_when_in_sync() {
        let plan = reconcile(
            &[desired("a", "version", DesiredState::Running)],
            &[remote("id1", "a", "version", LifecycleState::Running)],
            ReconcileOpts::default(),
        );
        assert!(plan.actions.is_empty());
        assert!(!plan.has_changes());
    }

    #[test]
    fn set_on_definition_drift() {
        let plan = reconcile(
            &[desired("a", "version | head", DesiredState::Running)],
            &[remote("id1", "a", "version", LifecycleState::Running)],
            ReconcileOpts::default(),
        );
        assert_eq!(plan.counts().set, 1);
        match &plan.actions[0] {
            Action::Set { remote, to } => {
                assert_eq!(remote.as_str(), "id1");
                assert_eq!(to.definition, "version | head");
            }
            other => panic!("expected Set, got {other:?}"),
        }
    }

    #[test]
    fn whitespace_only_diff_is_not_drift() {
        let plan = reconcile(
            &[desired("a", "  version \n", DesiredState::Running)],
            &[remote("id1", "a", "version", LifecycleState::Running)],
            ReconcileOpts::default(),
        );
        assert!(plan.actions.is_empty());
    }

    #[test]
    fn transition_on_state_mismatch() {
        let plan = reconcile(
            &[desired("a", "version", DesiredState::Paused)],
            &[remote("id1", "a", "version", LifecycleState::Running)],
            ReconcileOpts::default(),
        );
        assert_eq!(plan.counts().transition, 1);
        match &plan.actions[0] {
            Action::Transition { action, to, .. } => {
                assert_eq!(*action, TransitionAction::Pause);
                assert_eq!(*to, DesiredState::Paused);
            }
            other => panic!("expected Transition, got {other:?}"),
        }
    }

    #[test]
    fn drift_and_transition_together_are_ordered() {
        let plan = reconcile(
            &[desired("a", "v2", DesiredState::Paused)],
            &[remote("id1", "a", "v1", LifecycleState::Running)],
            ReconcileOpts::default(),
        );
        // set before transition.
        assert!(matches!(plan.actions[0], Action::Set { .. }));
        assert!(matches!(plan.actions[1], Action::Transition { .. }));
    }

    #[test]
    fn orphan_gated_without_prune() {
        let plan = reconcile(
            &[],
            &[remote("id1", "old", "version", LifecycleState::Running)],
            ReconcileOpts::default(),
        );
        assert!(plan.actions.is_empty());
        assert_eq!(plan.orphans.len(), 1);
        assert!(plan.has_changes());
    }

    #[test]
    fn orphan_deleted_with_prune() {
        let plan = reconcile(
            &[],
            &[remote("id1", "old", "version", LifecycleState::Running)],
            ReconcileOpts { prune: true },
        );
        assert_eq!(plan.counts().delete, 1);
        assert!(matches!(plan.actions[0], Action::Delete { .. }));
    }

    #[test]
    fn rename_is_delete_then_create_ordered() {
        // Local renamed "old" -> "new"; with prune, delete old + create new.
        let plan = reconcile(
            &[desired("new", "version", DesiredState::Running)],
            &[remote("id1", "old", "version", LifecycleState::Running)],
            ReconcileOpts { prune: true },
        );
        assert_eq!(plan.actions.len(), 2);
        assert!(matches!(plan.actions[0], Action::Delete { .. }));
        assert!(matches!(plan.actions[1], Action::Create(_)));
    }

    #[test]
    fn terminal_state_is_reported_not_forced() {
        let plan = reconcile(
            &[desired("a", "version", DesiredState::Running)],
            &[remote("id1", "a", "version", LifecycleState::Failed)],
            ReconcileOpts::default(),
        );
        // No transition forced; reported as terminal.
        assert!(plan.actions.is_empty());
        assert_eq!(plan.terminal.len(), 1);
        assert_eq!(plan.terminal[0].state, LifecycleState::Failed);
        // Terminal-only is not a "change".
        assert!(!plan.has_changes());
    }

    #[test]
    fn terminal_state_still_sets_on_drift() {
        let plan = reconcile(
            &[desired("a", "v2", DesiredState::Running)],
            &[remote("id1", "a", "v1", LifecycleState::Failed)],
            ReconcileOpts::default(),
        );
        assert_eq!(plan.counts().set, 1);
        assert_eq!(plan.terminal.len(), 1);
    }
}
