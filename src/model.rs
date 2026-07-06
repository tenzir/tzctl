//! Domain model: identifier newtypes and platform resources.
//!
//! Pipeline types (desired/remote/actions) are added in the read-path and
//! declarative-core stages; this stage establishes identifiers and the
//! workspace/node resources used by the platform client.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A workspace identifier (`t-xxxxxxxx`, the platform's `tenant_id`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TenantId(pub String);

impl TenantId {
    /// Borrow the underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TenantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for TenantId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// A node identifier (`n-xxxxxxxx`, the platform's `node_id`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub String);

impl NodeId {
    /// Borrow the underlying string.
    #[allow(dead_code)] // used by node-proxy URL construction (stage 4+).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for NodeId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// A pipeline identifier as assigned by the node.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PipelineId(pub String);

impl PipelineId {
    /// Borrow the underlying string.
    #[allow(dead_code)] // used by tests and lifecycle actions (stage 5+).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PipelineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A pipeline lifecycle state as observed on the node.
///
/// Unknown states (from a newer node) are preserved verbatim in [`Self::Other`]
/// so node-version drift never breaks parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleState {
    /// Created but not started.
    Created,
    /// Actively running.
    Running,
    /// Finished successfully.
    Completed,
    /// Terminated with an error.
    Failed,
    /// Paused; keeps in-memory state.
    Paused,
    /// Stopped; resets in-memory state.
    Stopped,
    /// An unrecognized state, displayed verbatim.
    Other(String),
}

impl LifecycleState {
    /// The canonical lowercase wire string for this state.
    pub fn as_wire(&self) -> &str {
        match self {
            LifecycleState::Created => "created",
            LifecycleState::Running => "running",
            LifecycleState::Completed => "completed",
            LifecycleState::Failed => "failed",
            LifecycleState::Paused => "paused",
            LifecycleState::Stopped => "stopped",
            LifecycleState::Other(s) => s,
        }
    }
}

impl LifecycleState {
    /// Map an observed lifecycle state to a declarable [`DesiredState`].
    ///
    /// Only `running`, `paused`, and the two terminal-but-declarable states
    /// (`stopped`/`completed`) map cleanly. Transient or error states
    /// (`created`, `failed`, unknown) return `None`, so callers writing
    /// frontmatter can fall back to the project default rather than pin a
    /// pipeline to a state it cannot hold.
    pub fn to_desired(&self) -> Option<DesiredState> {
        match self {
            LifecycleState::Running => Some(DesiredState::Running),
            LifecycleState::Paused => Some(DesiredState::Paused),
            LifecycleState::Stopped | LifecycleState::Completed => Some(DesiredState::Stopped),
            LifecycleState::Created | LifecycleState::Failed | LifecycleState::Other(_) => None,
        }
    }
}

impl From<&str> for LifecycleState {
    fn from(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "created" => LifecycleState::Created,
            "running" => LifecycleState::Running,
            "completed" => LifecycleState::Completed,
            "failed" => LifecycleState::Failed,
            "paused" => LifecycleState::Paused,
            "stopped" => LifecycleState::Stopped,
            _ => LifecycleState::Other(s.to_string()),
        }
    }
}

impl fmt::Display for LifecycleState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_wire())
    }
}

impl Serialize for LifecycleState {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for LifecycleState {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(LifecycleState::from(s.as_str()))
    }
}

/// A pipeline as it currently exists on the node.
///
/// Deserialization tolerates unknown/extra fields so node-version drift does
/// not break parsing. Note the node API has **no `description` field** (see
/// the pinned `pipeline_list.json` fixture); identity/drift keys on `name`,
/// `definition`, and `state`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemotePipeline {
    /// The node-assigned id.
    pub id: PipelineId,
    /// The user-assigned name (identity key).
    pub name: String,
    /// The TQL definition.
    #[serde(default)]
    pub definition: String,
    /// The current lifecycle state.
    pub state: LifecycleState,
    /// The last error message, if the pipeline failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The desired run-state of a pipeline, as declared by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DesiredState {
    /// The pipeline should be running (the default).
    #[default]
    Running,
    /// The pipeline should be paused (keeps in-memory state).
    Paused,
    /// The pipeline should be stopped (resets in-memory state).
    Stopped,
}

/// A run-state transition relayed to the node via `pipeline/update`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TransitionAction {
    /// Start (or resume) the pipeline.
    Start,
    /// Pause the pipeline, keeping in-memory state.
    Pause,
    /// Stop the pipeline, resetting in-memory state.
    Stop,
}

impl TransitionAction {
    /// The node `action` wire string.
    pub fn as_wire(&self) -> &'static str {
        match self {
            TransitionAction::Start => "start",
            TransitionAction::Pause => "pause",
            TransitionAction::Stop => "stop",
        }
    }
}

/// A pipeline as declared by the user (file-backed in this stage).
///
/// There is no `description`: the node API does not store one (see stage 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DesiredPipeline {
    /// The identity key (the pipeline name).
    pub name: String,
    /// The TQL definition.
    pub definition: String,
    /// The desired run-state.
    pub state: DesiredState,
    /// An optional explicit target node (overrides config).
    pub node: Option<NodeId>,
}

impl DesiredPipeline {
    /// The transitions needed to drive a freshly-created pipeline (in the
    /// `Created` state) to this desired state.
    pub fn create_transitions(&self) -> Vec<TransitionAction> {
        match self.state {
            // A created pipeline is not running; start it.
            DesiredState::Running => vec![TransitionAction::Start],
            // Start, then pause so it holds in-memory state.
            DesiredState::Paused => vec![TransitionAction::Start, TransitionAction::Pause],
            // Created is effectively stopped already; no transition needed.
            DesiredState::Stopped => vec![],
        }
    }
}

/// The minimal transition to move an observed `from` state toward `to`.
///
/// Returns `None` when the pipeline is already in the desired run-state.
#[allow(dead_code)] // consumed by the reconciler (stage 7).
pub fn transition_for(from: &LifecycleState, to: DesiredState) -> Option<TransitionAction> {
    match to {
        DesiredState::Running => match from {
            LifecycleState::Running => None,
            _ => Some(TransitionAction::Start),
        },
        DesiredState::Paused => match from {
            LifecycleState::Paused => None,
            _ => Some(TransitionAction::Pause),
        },
        DesiredState::Stopped => match from {
            LifecycleState::Stopped | LifecycleState::Completed => None,
            _ => Some(TransitionAction::Stop),
        },
    }
}

/// A workspace the authenticated user can access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// The workspace (tenant) id.
    pub tenant_id: TenantId,
    /// The human-readable workspace name.
    pub name: String,
}

/// A node within a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// The node id.
    pub node_id: NodeId,
    /// The human-readable node name.
    pub name: String,
    /// Whether the node is currently connected to the platform.
    pub connected: bool,
    /// The raw lifecycle state reported by the platform, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_state: Option<String>,
}

/// An error from resolving a user-supplied identifier to a single item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// No item matched the query.
    NotFound(String),
    /// The query matched more than one item.
    Ambiguous {
        /// The query that matched multiple items.
        query: String,
        /// Human-readable labels of the matches.
        matches: Vec<String>,
    },
    /// A numeric index was out of range.
    IndexOutOfRange {
        /// The 1-based index requested.
        index: usize,
        /// The number of available items.
        len: usize,
    },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::NotFound(q) => write!(f, "no match for {q:?}"),
            ResolveError::Ambiguous { query, matches } => {
                write!(f, "{query:?} is ambiguous; matches: {}", matches.join(", "))
            }
            ResolveError::IndexOutOfRange { index, len } => {
                write!(f, "index {index} is out of range (1..={len})")
            }
        }
    }
}

/// Resolve a query against `items` by id, exact name, or 1-based index.
///
/// `id_of` and `name_of` extract the comparable fields. A query that looks
/// like a `t-`/`n-` id (or otherwise equals an item's id) wins; otherwise an
/// exact name match is tried; otherwise a purely numeric query is treated as a
/// 1-based index. Returns the matched item's position.
pub fn resolve_index<T>(
    items: &[T],
    query: &str,
    id_of: impl Fn(&T) -> &str,
    name_of: impl Fn(&T) -> &str,
) -> Result<usize, ResolveError> {
    // 1. Exact id match.
    let id_matches: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, it)| id_of(it) == query)
        .map(|(i, _)| i)
        .collect();
    if id_matches.len() == 1 {
        return Ok(id_matches[0]);
    }
    // 2. Exact name match.
    let name_matches: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, it)| name_of(it) == query)
        .map(|(i, _)| i)
        .collect();
    match name_matches.len() {
        1 => return Ok(name_matches[0]),
        n if n > 1 => {
            return Err(ResolveError::Ambiguous {
                query: query.to_string(),
                matches: name_matches
                    .iter()
                    .map(|&i| id_of(&items[i]).to_string())
                    .collect(),
            });
        }
        _ => {}
    }
    // 3. Numeric index (1-based).
    if let Ok(index) = query.parse::<usize>() {
        if index == 0 || index > items.len() {
            return Err(ResolveError::IndexOutOfRange {
                index,
                len: items.len(),
            });
        }
        return Ok(index - 1);
    }
    Err(ResolveError::NotFound(query.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(id: &str, name: &str) -> Workspace {
        Workspace {
            tenant_id: TenantId(id.to_string()),
            name: name.to_string(),
        }
    }

    fn resolve(items: &[Workspace], q: &str) -> Result<usize, ResolveError> {
        resolve_index(items, q, |w| w.tenant_id.as_str(), |w| w.name.as_str())
    }

    #[test]
    fn resolves_by_id() {
        let items = [ws("t-aaaa1111", "prod"), ws("t-bbbb2222", "dev")];
        assert_eq!(resolve(&items, "t-bbbb2222").unwrap(), 1);
    }

    #[test]
    fn resolves_by_name() {
        let items = [ws("t-aaaa1111", "prod"), ws("t-bbbb2222", "dev")];
        assert_eq!(resolve(&items, "prod").unwrap(), 0);
    }

    #[test]
    fn resolves_by_index() {
        let items = [ws("t-aaaa1111", "prod"), ws("t-bbbb2222", "dev")];
        assert_eq!(resolve(&items, "2").unwrap(), 1);
    }

    #[test]
    fn index_out_of_range() {
        let items = [ws("t-aaaa1111", "prod")];
        assert!(matches!(
            resolve(&items, "5"),
            Err(ResolveError::IndexOutOfRange { index: 5, len: 1 })
        ));
        assert!(matches!(
            resolve(&items, "0"),
            Err(ResolveError::IndexOutOfRange { .. })
        ));
    }

    #[test]
    fn ambiguous_name() {
        let items = [ws("t-aaaa1111", "dup"), ws("t-bbbb2222", "dup")];
        assert!(matches!(
            resolve(&items, "dup"),
            Err(ResolveError::Ambiguous { .. })
        ));
    }

    #[test]
    fn not_found() {
        let items = [ws("t-aaaa1111", "prod")];
        assert!(matches!(
            resolve(&items, "nope"),
            Err(ResolveError::NotFound(_))
        ));
    }

    #[test]
    fn create_transitions_by_desired_state() {
        let mk = |state| DesiredPipeline {
            name: "p".into(),
            definition: "version".into(),
            state,
            node: None,
        };
        assert_eq!(
            mk(DesiredState::Running).create_transitions(),
            vec![TransitionAction::Start]
        );
        assert_eq!(
            mk(DesiredState::Paused).create_transitions(),
            vec![TransitionAction::Start, TransitionAction::Pause]
        );
        assert!(mk(DesiredState::Stopped).create_transitions().is_empty());
    }

    #[test]
    fn transition_for_is_minimal() {
        use LifecycleState as L;
        // No-ops when already in the target state.
        assert_eq!(transition_for(&L::Running, DesiredState::Running), None);
        assert_eq!(transition_for(&L::Paused, DesiredState::Paused), None);
        assert_eq!(transition_for(&L::Stopped, DesiredState::Stopped), None);
        assert_eq!(transition_for(&L::Completed, DesiredState::Stopped), None);
        // Otherwise the matching action.
        assert_eq!(
            transition_for(&L::Paused, DesiredState::Running),
            Some(TransitionAction::Start)
        );
        assert_eq!(
            transition_for(&L::Running, DesiredState::Paused),
            Some(TransitionAction::Pause)
        );
        assert_eq!(
            transition_for(&L::Running, DesiredState::Stopped),
            Some(TransitionAction::Stop)
        );
    }

    #[test]
    fn lifecycle_state_to_desired() {
        use LifecycleState as L;
        assert_eq!(L::Running.to_desired(), Some(DesiredState::Running));
        assert_eq!(L::Paused.to_desired(), Some(DesiredState::Paused));
        assert_eq!(L::Stopped.to_desired(), Some(DesiredState::Stopped));
        assert_eq!(L::Completed.to_desired(), Some(DesiredState::Stopped));
        assert_eq!(L::Created.to_desired(), None);
        assert_eq!(L::Failed.to_desired(), None);
        assert_eq!(L::Other("weird".into()).to_desired(), None);
    }

    #[test]
    fn lifecycle_state_mapping() {
        assert_eq!(LifecycleState::from("running"), LifecycleState::Running);
        assert_eq!(LifecycleState::from("PAUSED"), LifecycleState::Paused);
        assert_eq!(LifecycleState::from("stopped"), LifecycleState::Stopped);
        // Unknown states are preserved verbatim.
        match LifecycleState::from("quantum") {
            LifecycleState::Other(s) => assert_eq!(s, "quantum"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn lifecycle_state_serde_round_trip() {
        for wire in [
            "created",
            "running",
            "completed",
            "failed",
            "paused",
            "stopped",
            "weird",
        ] {
            let json = format!("\"{wire}\"");
            let state: LifecycleState = serde_json::from_str(&json).unwrap();
            assert_eq!(serde_json::to_string(&state).unwrap(), json);
        }
    }

    #[test]
    fn remote_pipeline_ignores_unknown_fields() {
        let json = r#"{
            "id": "abc", "name": "p", "definition": "version",
            "state": "running", "future_field": 42, "diagnostics": []
        }"#;
        let p: RemotePipeline = serde_json::from_str(json).unwrap();
        assert_eq!(p.name, "p");
        assert_eq!(p.state, LifecycleState::Running);
    }

    #[test]
    fn id_takes_precedence_over_index() {
        // A name that is also numeric should match the name, not the index.
        let items = [ws("t-aaaa1111", "1"), ws("t-bbbb2222", "two")];
        // Query "1" matches name "1" at index 0, not 1-based index 1.
        assert_eq!(resolve(&items, "1").unwrap(), 0);
    }
}
