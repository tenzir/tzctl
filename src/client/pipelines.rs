//! Wire types for the node `pipeline/*` endpoints.

use serde::{Deserialize, Serialize};

use crate::model::{DesiredPipeline, PipelineId, RemotePipeline, TransitionAction};

/// The `pipeline/list` request body.
///
/// The node expects an empty object; a unit struct serializes to `{}`.
#[derive(Debug, Default, Serialize)]
pub struct ListRequest {}

/// The `pipeline/list` response body.
#[derive(Debug, Deserialize)]
pub struct ListResponse {
    /// The pipelines currently known to the node.
    #[serde(default)]
    pub pipelines: Vec<RemotePipeline>,
}

/// The `pipeline/create` request body.
#[derive(Debug, Serialize)]
pub struct CreateRequest<'a> {
    /// The user-assigned name.
    pub name: &'a str,
    /// The TQL definition.
    pub definition: &'a str,
    /// Whether the pipeline is hidden in the UI (always `false` for `tz`).
    pub hidden: bool,
}

impl<'a> CreateRequest<'a> {
    /// Build a create request from a desired pipeline.
    pub fn from_desired(p: &'a DesiredPipeline) -> Self {
        Self {
            name: &p.name,
            definition: &p.definition,
            hidden: false,
        }
    }
}

/// The `pipeline/create` response body.
#[derive(Debug, Deserialize)]
pub struct CreateResponse {
    /// The id of the newly-created pipeline.
    pub id: PipelineId,
}

/// The `pipeline/update` request body (only `id` is required).
#[derive(Debug, Serialize)]
pub struct UpdateRequest<'a> {
    /// The id of the pipeline to update.
    pub id: &'a str,
    /// A new definition, for in-place edits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<&'a str>,
    /// A new name, for in-place renames.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<&'a str>,
    /// A run-state action (`start`/`pause`/`stop`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<&'static str>,
}

impl<'a> UpdateRequest<'a> {
    /// An update that only changes the run-state.
    pub fn action(id: &'a str, action: TransitionAction) -> Self {
        Self {
            id,
            definition: None,
            name: None,
            action: Some(action.as_wire()),
        }
    }

    /// An in-place edit of `name`/`definition` (no state change).
    pub fn edit(id: &'a str, p: &'a DesiredPipeline) -> Self {
        Self {
            id,
            definition: Some(&p.definition),
            name: Some(&p.name),
            action: None,
        }
    }
}

/// The `pipeline/delete` request body.
#[derive(Debug, Serialize)]
pub struct DeleteRequest<'a> {
    /// The id of the pipeline to delete.
    pub id: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LifecycleState;

    #[test]
    fn list_request_serializes_to_empty_object() {
        assert_eq!(serde_json::to_string(&ListRequest {}).unwrap(), "{}");
    }

    #[test]
    fn create_response_parses_fixture() {
        let raw = include_str!("../../tests/fixtures/pipeline_create.json");
        let resp: CreateResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.id.as_str(), "4c7f2b11-6169-4d1b-89b4-4fc0a68b3d4a");
    }

    #[test]
    fn update_action_body_is_minimal() {
        let body =
            serde_json::to_value(UpdateRequest::action("abc", TransitionAction::Pause)).unwrap();
        assert_eq!(body, serde_json::json!({"id": "abc", "action": "pause"}));
    }

    #[test]
    fn update_edit_body_sets_definition_and_name() {
        let p = crate::model::DesiredPipeline {
            name: "p".to_string(),
            definition: "version".to_string(),
            state: crate::model::DesiredState::Running,
            node: None,
        };
        let body = serde_json::to_value(UpdateRequest::edit("id1", &p)).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"id": "id1", "definition": "version", "name": "p"})
        );
    }

    #[test]
    fn delete_body() {
        let body = serde_json::to_value(DeleteRequest { id: "x" }).unwrap();
        assert_eq!(body, serde_json::json!({"id": "x"}));
    }

    #[test]
    fn parses_pinned_fixture() {
        let raw = include_str!("../../tests/fixtures/pipeline_list.json");
        let resp: ListResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.pipelines.len(), 2);

        let first = &resp.pipelines[0];
        assert_eq!(first.name, "user-assigned-name");
        assert_eq!(first.definition, "export | where foo | publish /bar");
        assert_eq!(first.state, LifecycleState::Running);
        assert_eq!(first.id.as_str(), "4c7f2b11-6169-4d1b-89b4-4fc0a68b3d4a");
        assert!(first.error.is_none());

        let second = &resp.pipelines[1];
        assert_eq!(second.state, LifecycleState::Failed);
        assert_eq!(second.error.as_deref(), Some("format 'asdf' not found"));
    }

    #[test]
    fn json_round_trips_through_model() {
        let raw = include_str!("../../tests/fixtures/pipeline_list.json");
        let resp: ListResponse = serde_json::from_str(raw).unwrap();
        let json = serde_json::to_string(&resp.pipelines).unwrap();
        let back: Vec<RemotePipeline> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, resp.pipelines);
    }
}
