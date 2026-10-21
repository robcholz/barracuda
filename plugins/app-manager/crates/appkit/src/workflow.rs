use crate::bundle::AppList;
use crate::manager::{AppError, AppExit, AppManager, RunningApp};
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
pub(crate) struct AppRequest {
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InteractiveRequest {
    interactive: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TapRequest {
    x: f32,
    y: f32,
}

#[derive(Serialize)]
pub(crate) struct AppResponse {
    app: Option<RunningApp>,
}

#[derive(Serialize)]
pub(crate) struct StatusResponse {
    foreground: Option<RunningApp>,
    running: Vec<RunningApp>,
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
    Error {
        error: AppError,
        /// Why an incompatible app cannot run on this device.
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl<T> From<Result<T, AppError>> for Response<T> {
    fn from(result: Result<T, AppError>) -> Self {
        match result {
            Ok(response) => Self::Success(response),
            Err(error) => Self::Error {
                error,
                reason: None,
            },
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

struct OpenAction(AppManager);

impl WorkflowActionHandler for OpenAction {
    type Request = AppRequest;
    type Response = Response<RunningApp>;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("app.open");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        Box::pin(async move {
            Ok(match self.0.open(&request.id).await {
                Err(AppError::Incompatible) => Response::Error {
                    error: AppError::Incompatible,
                    reason: self
                        .0
                        .list()
                        .await
                        .apps
                        .into_iter()
                        .find(|app| app.id == request.id)
                        .and_then(|app| app.incompatible),
                },
                opened => opened.into(),
            })
        })
    }
}

struct HomeAction(AppManager);

impl WorkflowActionHandler for HomeAction {
    type Request = Empty;
    type Response = AppResponse;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("app.home");

    fn invoke(&self, _request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let app = self.0.home();
        Box::pin(async move { Ok(AppResponse { app }) })
    }
}

struct InteractiveAction(AppManager);

impl WorkflowActionHandler for InteractiveAction {
    type Request = InteractiveRequest;
    type Response = Response<RunningApp>;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("app.interactive");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let result = self.0.set_interactive(request.interactive);
        Box::pin(async move { Ok(result.into()) })
    }
}

struct TerminateAction(AppManager);

impl WorkflowActionHandler for TerminateAction {
    type Request = AppRequest;
    type Response = AppResponse;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("app.terminate");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let app = self.0.terminate(&request.id);
        Box::pin(async move { Ok(AppResponse { app }) })
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
            running: self.0.running(),
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

/// Registers the `app.*` Workflow Actions on `manager`; keep the registrations.
///
/// # Errors
///
/// Fails when an Action cannot be registered.
pub fn register_actions(
    actions: &WorkflowActionRegistry,
    manager: &AppManager,
) -> Result<Vec<WorkflowActionRegistration>, WorkflowActionRegistryError> {
    Ok(alloc::vec![
        actions.add_action(ListAction(manager.clone()))?,
        actions.add_action(OpenAction(manager.clone()))?,
        actions.add_action(HomeAction(manager.clone()))?,
        actions.add_action(InteractiveAction(manager.clone()))?,
        actions.add_action(TerminateAction(manager.clone()))?,
        actions.add_action(StatusAction(manager.clone()))?,
        actions.add_action(TapAction(manager.clone()))?,
    ])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::bundle::{AppInfo, AppKind, AppLocation, InvalidApp};
    use crate::port::AppState;
    use alloc::vec;
    use barracuda_vm_plugin::{VmExecutionError, VmRunOutcome};
    use barracuda_workflow_plugin::WorkflowActionHandler;
    use serde_json::{Value, json};

    fn running(state: AppState) -> RunningApp {
        RunningApp {
            app: AppInfo {
                id: "counter".into(),
                name: "Counter".into(),
                version: "0.1.0".into(),
                location: AppLocation::Workspace,
                kind: AppKind::Installable,
                has_view: true,
                api: 1,
                requires: vec!["light".into()],
                uses: vec!["http".into()],
                permissions: vec!["gpio".into()],
                boards: vec!["espressif-esp-vocat-v1-2".into()],
                view_size: Some([360, 360]),
                view_target: None,
                icon: Some("lamp".into()),
                incompatible: Some("This device has no light".into()),
            },
            run_id: 3,
            state,
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
            apps: vec![running(AppState::Active).app],
            invalid: vec![InvalidApp {
                location: AppLocation::Workspace,
                dir: "Draft.bapp".into(),
                problem: "app.json is missing".into(),
            }],
        });
        check::<OpenAction>(Response::from(Ok::<_, AppError>(running(AppState::Active))));
        for error in [AppError::NotFound, AppError::InvalidBundle, AppError::Busy] {
            check::<OpenAction>(Response::<RunningApp>::from(Err(error)));
        }
        check::<OpenAction>(Response::<RunningApp>::Error {
            error: AppError::Incompatible,
            reason: Some("Only for espressif-esp-vocat-v1-2".into()),
        });
        check::<HomeAction>(AppResponse { app: None });
        check::<HomeAction>(AppResponse {
            app: Some(running(AppState::Background)),
        });
        check::<InteractiveAction>(Response::from(Ok::<_, AppError>(running(
            AppState::Inactive,
        ))));
        check::<InteractiveAction>(Response::<RunningApp>::from(Err(AppError::NoForegroundApp)));
        check::<TerminateAction>(AppResponse {
            app: Some(running(AppState::Terminating)),
        });
        check::<StatusAction>(StatusResponse {
            foreground: None,
            running: Vec::new(),
            texts: None,
            last_exit: None,
        });
        check::<StatusAction>(StatusResponse {
            foreground: Some(running(AppState::Active)),
            running: vec![running(AppState::Active), running(AppState::Suspended)],
            texts: Some(vec!["0".into(), "+1".into()]),
            last_exit: Some(AppExit {
                id: "broken".into(),
                run_id: 2,
                outcome: VmRunOutcome::Error,
                error: Some(VmExecutionError::LuaRuntime),
                diagnostic: Some("boom".into()),
                output: vec!["starting".into()],
                foreground: true,
            }),
        });
        check::<TapAction>(Response::from(Ok::<_, AppError>(TapResponse {
            handler: Some("increment".into()),
        })));
        check::<TapAction>(Response::from(Ok::<_, AppError>(TapResponse {
            handler: None,
        })));
        for error in [AppError::NoForegroundApp, AppError::NotInteractive] {
            check::<TapAction>(Response::<TapResponse>::from(Err(error)));
        }
    }

    #[test]
    fn requests_are_validated() {
        assert!(request::<ListAction>(json!({})));
        assert!(request::<OpenAction>(json!({ "id": "counter" })));
        assert!(!request::<OpenAction>(json!({})));
        assert!(request::<InteractiveAction>(
            json!({ "interactive": false })
        ));
        assert!(!request::<InteractiveAction>(json!({})));
        assert!(request::<TerminateAction>(json!({ "id": "counter" })));
        assert!(request::<TapAction>(json!({ "x": 12.5, "y": 40 })));
        assert!(!request::<TapAction>(json!({ "x": 12.5 })));
        assert!(!request::<HomeAction>(json!({ "id": "counter" })));
    }
}
