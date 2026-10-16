use crate::agent::{AgentState, AgentStatus};
use crate::notices::Notifications;
use alloc::{boxed::Box, rc::Rc, string::String, vec::Vec};
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
    #[serde(default)]
    icon: Option<String>,
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

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("app.notify");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let id = self
            .0
            .post(&request.title, &request.body, request.icon.as_deref());
        Box::pin(async move { Ok(NotifyResponse { id }) })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfirmRequest {
    title: String,
    #[serde(default)]
    body: String,
}

#[derive(Serialize)]
pub(crate) struct ConfirmResponse {
    allowed: bool,
    answered: bool,
}

/// Asks the person to allow or deny something and waits for the answer.
pub(crate) struct ConfirmAction(pub(crate) Rc<Notifications>);

impl WorkflowActionHandler for ConfirmAction {
    type Request = ConfirmRequest;
    type Response = ConfirmResponse;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("app.confirm");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let id = self.0.ask(&request.title, &request.body);
        Box::pin(async move {
            let answer = self.0.answer(id).await;
            Ok(ConfirmResponse {
                allowed: answer == Some(true),
                answered: answer.is_some(),
            })
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentRequest {
    state: AgentState,
    #[serde(default)]
    question: String,
    #[serde(default)]
    answer: String,
}

#[derive(Serialize)]
pub(crate) struct AgentResponse {}

/// Reports the Agent's state for the home screen.
pub(crate) struct AgentAction(pub(crate) Rc<AgentStatus>);

impl WorkflowActionHandler for AgentAction {
    type Request = AgentRequest;
    type Response = AgentResponse;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("app.agent");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        self.0
            .set(request.state, &request.question, &request.answer);
        Box::pin(async move { Ok(AgentResponse {}) })
    }
}

pub(crate) fn register_actions(
    actions: &WorkflowActionRegistry,
    notices: &Rc<Notifications>,
    agent: &Rc<AgentStatus>,
) -> Result<Vec<WorkflowActionRegistration>, WorkflowActionRegistryError> {
    Ok(alloc::vec![
        actions.add_action(NotifyAction(Rc::clone(notices)))?,
        actions.add_action(ConfirmAction(Rc::clone(notices)))?,
        actions.add_action(AgentAction(Rc::clone(agent)))?,
    ])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use futures_lite::future::block_on;
    use serde_json::json;

    fn valid<A: WorkflowActionHandler>(response: &impl Serialize) {
        let value = serde_json::to_value(response).unwrap();
        assert!(A::SCHEMA.response().validate(&value).is_ok(), "{value}");
    }

    #[test]
    fn notify_posts_a_notice_and_matches_its_schemas() {
        let schema = NotifyAction::SCHEMA;
        assert!(schema.request().validate(&json!({ "title": "hi" })).is_ok());
        assert!(
            schema
                .request()
                .validate(&json!({ "title": "hi", "icon": "lamp" }))
                .is_ok()
        );
        assert!(schema.request().validate(&json!({ "body": "x" })).is_err());
        let notices = Rc::new(Notifications::new());
        let action = NotifyAction(Rc::clone(&notices));
        let response = block_on(action.invoke(NotifyRequest {
            title: "门铃".into(),
            body: "有人在门口".into(),
            icon: Some("lamp".into()),
        }))
        .unwrap();
        valid::<NotifyAction>(&response);
        assert_eq!(notices.list()[0].title, "门铃");
        assert_eq!(notices.list()[0].icon.as_deref(), Some("lamp"));
        assert!(notices.unread());
    }

    #[test]
    fn confirm_waits_for_the_answer() {
        let notices = Rc::new(Notifications::new());
        let action = ConfirmAction(Rc::clone(&notices));
        assert!(
            ConfirmAction::SCHEMA
                .request()
                .validate(&json!({ "title": "打开客厅灯带？", "body": "林夏 · Telegram" }))
                .is_ok()
        );
        let pending = action.invoke(ConfirmRequest {
            title: "打开客厅灯带？".into(),
            body: String::new(),
        });
        let question = notices.list()[0].clone();
        assert!(question.question);
        assert!(notices.decide(question.id, true));
        let response = block_on(pending).unwrap();
        assert!(response.allowed && response.answered);
        valid::<ConfirmAction>(&response);
        assert!(notices.list().is_empty());

        let dismissed = action.invoke(ConfirmRequest {
            title: "删除文件？".into(),
            body: String::new(),
        });
        notices.dismiss(notices.list()[0].id);
        let response = block_on(dismissed).unwrap();
        assert!(!response.allowed && !response.answered);
    }

    #[test]
    fn agent_reports_the_home_screen_state() {
        let agent = Rc::new(AgentStatus::new());
        let action = AgentAction(Rc::clone(&agent));
        let request = json!({ "state": "answering", "question": "明天？", "answer": "9:30 开会" });
        assert!(AgentAction::SCHEMA.request().validate(&request).is_ok());
        assert!(
            AgentAction::SCHEMA
                .request()
                .validate(&json!({ "state": "dancing" }))
                .is_err()
        );
        let response = block_on(action.invoke(serde_json::from_value(request).unwrap())).unwrap();
        valid::<AgentAction>(&response);
        assert_eq!(agent.state(), AgentState::Answering);
        assert_eq!(agent.answer(), "9:30 开会");
    }
}
