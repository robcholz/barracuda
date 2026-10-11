use crate::bundle::AppList;
use crate::manager::{AppError, AppExit, AppManager, ForegroundApp};
use alloc::{boxed::Box, string::String, vec::Vec};
use barracuda_workflow_plugin::{
    WorkflowActionFuture, WorkflowActionHandler, WorkflowActionRegistration,
    WorkflowActionRegistry, WorkflowActionRegistryError, WorkflowActionSchema,
    workflow_action_schema,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Empty {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LaunchRequest {
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TapRequest {
    x: f32,
    y: f32,
}

#[derive(Serialize)]
pub(crate) struct StopResponse {
    stopped: Option<ForegroundApp>,
}

#[derive(Serialize)]
pub(crate) struct StatusResponse {
    foreground: Option<ForegroundApp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    texts: Option<Vec<String>>,
    last_exit: Option<AppExit>,
}

#[derive(Serialize)]
pub(crate) struct TapResponse {
    handler: Option<String>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum Response<T> {
    Success(T),
    Error { error: AppError },
}

impl<T> From<Result<T, AppError>> for Response<T> {
    fn from(result: Result<T, AppError>) -> Self {
        match result {
            Ok(response) => Self::Success(response),
            Err(error) => Self::Error { error },
        }
    }
}

struct ListAction(AppManager);

impl WorkflowActionHandler for ListAction {
    type Request = Empty;
    type Response = AppList;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("app.list");

    fn invoke(&self, _request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        Box::pin(async move { Ok(self.0.list().await) })
    }
}

struct LaunchAction(AppManager);

impl WorkflowActionHandler for LaunchAction {
    type Request = LaunchRequest;
    type Response = Response<ForegroundApp>;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("app.launch");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        Box::pin(async move { Ok(self.0.launch(&request.id).await.into()) })
    }
}

struct StopAction(AppManager);

impl WorkflowActionHandler for StopAction {
    type Request = Empty;
    type Response = StopResponse;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("app.stop");

    fn invoke(&self, _request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let stopped = self.0.stop();
        Box::pin(async move { Ok(StopResponse { stopped }) })
    }
}

struct StatusAction(AppManager);

impl WorkflowActionHandler for StatusAction {
    type Request = Empty;
    type Response = StatusResponse;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("app.status");

    fn invoke(&self, _request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let response = StatusResponse {
            foreground: self.0.foreground(),
            texts: self.0.texts().ok(),
            last_exit: self.0.last_exit(),
        };
        Box::pin(async move { Ok(response) })
    }
}

struct TapAction(AppManager);

impl WorkflowActionHandler for TapAction {
    type Request = TapRequest;
    type Response = Response<TapResponse>;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("app.tap");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let result = self
            .0
            .tap(request.x, request.y)
            .map(|handler| TapResponse { handler });
        Box::pin(async move { Ok(result.into()) })
    }
}

pub(crate) fn register_actions(
    actions: &WorkflowActionRegistry,
    manager: &AppManager,
) -> Result<Vec<WorkflowActionRegistration>, WorkflowActionRegistryError> {
    Ok(alloc::vec![
        actions.add_action(ListAction(manager.clone()))?,
        actions.add_action(LaunchAction(manager.clone()))?,
        actions.add_action(StopAction(manager.clone()))?,
        actions.add_action(StatusAction(manager.clone()))?,
        actions.add_action(TapAction(manager.clone()))?,
    ])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::bundle::{AppInfo, AppLocation, InvalidApp};
    use alloc::vec;
    use barracuda_vm_plugin::{VmExecutionError, VmRunOutcome};
    use barracuda_workflow_plugin::WorkflowActionHandler;
    use serde_json::{Value, json};

    fn app(has_view: bool) -> AppInfo {
        AppInfo {
            id: "counter".into(),
            name: "计数器".into(),
            version: "0.1.0".into(),
            location: AppLocation::Bundled,
            has_view,
        }
    }

    fn foreground() -> ForegroundApp {
        ForegroundApp {
            app: app(true),
            run_id: 3,
        }
    }

    fn check<A: WorkflowActionHandler>(response: impl Serialize) {
        let value = serde_json::to_value(response).unwrap();
        A::SCHEMA
            .response()
            .validate(&value)
            .unwrap_or_else(|error| panic!("{}: {value}: {error:?}", A::SCHEMA.address()));
    }

    fn request<A: WorkflowActionHandler>(value: Value) -> bool {
        A::SCHEMA.request().validate(&value).is_ok()
    }

    #[test]
    fn responses_match_their_schemas() {
        check::<ListAction>(AppList::default());
        check::<ListAction>(AppList {
            apps: vec![app(true), app(false)],
            invalid: vec![InvalidApp {
                location: AppLocation::Workspace,
                dir: "Draft".into(),
                problem: "app.json is missing".into(),
            }],
        });
        check::<LaunchAction>(Response::from(Ok::<_, AppError>(foreground())));
        for error in [AppError::NotFound, AppError::InvalidBundle, AppError::Busy] {
            check::<LaunchAction>(Response::<ForegroundApp>::from(Err(error)));
        }
        check::<StopAction>(StopResponse { stopped: None });
        check::<StopAction>(StopResponse {
            stopped: Some(foreground()),
        });
        check::<StatusAction>(StatusResponse {
            foreground: None,
            texts: None,
            last_exit: None,
        });
        check::<StatusAction>(StatusResponse {
            foreground: Some(foreground()),
            texts: Some(vec!["0".into(), "+1".into()]),
            last_exit: Some(AppExit {
                id: "broken".into(),
                run_id: 2,
                outcome: VmRunOutcome::Error,
                error: Some(VmExecutionError::LuaRuntime),
                diagnostic: Some("boom".into()),
                output: vec!["starting".into()],
            }),
        });
        check::<TapAction>(Response::from(Ok::<_, AppError>(TapResponse {
            handler: Some("increment".into()),
        })));
        check::<TapAction>(Response::from(Ok::<_, AppError>(TapResponse {
            handler: None,
        })));
        check::<TapAction>(Response::<TapResponse>::from(Err(AppError::NoView)));
    }

    #[test]
    fn requests_are_validated() {
        assert!(request::<ListAction>(json!({})));
        assert!(request::<LaunchAction>(json!({ "id": "counter" })));
        assert!(!request::<LaunchAction>(json!({})));
        assert!(request::<TapAction>(json!({ "x": 12.5, "y": 40 })));
        assert!(!request::<TapAction>(json!({ "x": 12.5 })));
        assert!(!request::<StopAction>(json!({ "id": "counter" })));
    }
}
