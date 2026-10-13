use crate::notices::Notifications;
use alloc::{boxed::Box, rc::Rc, string::String};
use barracuda_workflow_plugin::{
    WorkflowActionFuture, WorkflowActionHandler, WorkflowActionRegistration,
    WorkflowActionRegistry, WorkflowActionRegistryError, WorkflowActionSchema,
    workflow_action_schema,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NotifyRequest {
    title: String,
    #[serde(default)]
    body: String,
}

#[derive(Serialize)]
pub(crate) struct NotifyResponse {
    id: u32,
}

/// Posts a notice to the notification center.
pub(crate) struct NotifyAction(pub(crate) Rc<Notifications>);

impl WorkflowActionHandler for NotifyAction {
    type Request = NotifyRequest;
    type Response = NotifyResponse;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("shell.notify");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let id = self.0.post(&request.title, &request.body);
        Box::pin(async move { Ok(NotifyResponse { id }) })
    }
}

pub(crate) fn register_actions(
    actions: &WorkflowActionRegistry,
    notices: &Rc<Notifications>,
) -> Result<WorkflowActionRegistration, WorkflowActionRegistryError> {
    actions.add_action(NotifyAction(Rc::clone(notices)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    #[test]
    fn notify_posts_a_notice_and_matches_its_schemas() {
        let schema = NotifyAction::SCHEMA;
        assert!(schema.request().validate(&json!({ "title": "hi" })).is_ok());
        assert!(schema.request().validate(&json!({ "body": "x" })).is_err());
        let notices = Rc::new(Notifications::new());
        let action = NotifyAction(Rc::clone(&notices));
        let response = futures_lite::future::block_on(action.invoke(NotifyRequest {
            title: "门铃".into(),
            body: "有人在门口".into(),
        }))
        .unwrap();
        let value = serde_json::to_value(&response).unwrap();
        assert!(schema.response().validate(&value).is_ok());
        assert_eq!(notices.list()[0].title, "门铃");
        assert!(notices.unread());
    }
}
