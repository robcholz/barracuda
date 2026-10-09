use alloc::{boxed::Box, rc::Rc, string::String};

use barracuda_plugin::manager::PluginStorage;
use barracuda_workflow_plugin::{
    WorkflowActionFuture, WorkflowActionHandler, WorkflowActionSchema, workflow_action_schema,
};
use embassy_futures::select::select;
use embassy_time::{Instant, Timer};
use serde::{Deserialize, Serialize};

use crate::{
    bridge::{BridgeControl, BridgeShared},
    state::{BridgeError, Resolution, ResolveResult, Route, persist_mapping, validate_message_id},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RouteRequest {
    channel: String,
    conversation_id: String,
    #[serde(default)]
    thread_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum ToAgentRequest {
    Resolve {
        route: RouteRequest,
        message_id: String,
        text: String,
    },
    Bind {
        route: RouteRequest,
        message_id: String,
        session: String,
    },
}

#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum ToAgentResponse {
    Missing {},
    Resolved {
        open_required: bool,
        session: String,
    },
    Bound {
        session: String,
        /// The open input request this message answers with `session.respond`.
        #[serde(skip_serializing_if = "Option::is_none")]
        input_request: Option<String>,
    },
    Error {
        error: BridgeError,
    },
}

fn route(request: RouteRequest) -> Result<Route, BridgeError> {
    Route::new(
        &request.channel,
        &request.conversation_id,
        request.thread_id.as_deref(),
    )
}

/// Resolves `route`, reserving it when unmapped. While another inbound
/// message holds the reservation, waits for it to bind (or lapse) so this
/// message joins that session instead of creating a second one.
async fn resolve<Storage>(shared: &BridgeShared<Storage>, route: &Route) -> ResolveResult {
    loop {
        let mut book = shared.book.lock().await;
        let until = match book.resolve_or_reserve(route, Instant::now()) {
            Resolution::Ready(found) => return found,
            Resolution::Wait { until } => until,
        };
        let generation = shared.routes.generation();
        drop(book);
        log::info!(
            "IMessage Bridge waiting for `{}` conversation `{}` to bind its new session",
            route.channel,
            route.conversation_id
        );
        select(shared.routes.changed_since(generation), Timer::at(until)).await;
    }
}

/// Binds `session` to `route` for one inbound message and returns the open
/// input request it answers. Either way the route's reservation ends and
/// waiting messages resolve again: to the new mapping, or, after a failure,
/// one of them reserves the route and creates the session itself.
async fn bind<Storage>(
    shared: &BridgeShared<Storage>,
    route: Route,
    message_id: &str,
    session: &str,
) -> Result<Option<String>, BridgeError>
where
    Storage: PluginStorage,
{
    let mut book = shared.book.lock().await;
    let bound = match book.prepare_binding(route.clone(), message_id, session) {
        Ok((mapping, input_request)) => match persist_mapping(&shared.storage, &mapping).await {
            Ok(()) => {
                book.commit_mapping(mapping);
                Ok(input_request)
            }
            Err(error) => {
                log::warn!(
                    "IMessage Bridge failed to persist binding for `{session}`: {}",
                    error.code()
                );
                Err(error)
            }
        },
        Err(error) => Err(error),
    };
    if bound.is_err() {
        book.release(&route);
    }
    shared.routes.notify();
    bound
}

async fn invoke<Storage>(shared: &BridgeShared<Storage>, request: ToAgentRequest) -> ToAgentResponse
where
    Storage: PluginStorage,
{
    let result = match request {
        ToAgentRequest::Resolve {
            route: route_request,
            message_id,
            text,
        } => {
            let _complete_message = text;
            log::info!(
                "IMessage Bridge resolving `{}` conversation `{}` for inbound message `{message_id}`",
                route_request.channel,
                route_request.conversation_id
            );
            match route(route_request) {
                Ok(route) => match validate_message_id(&message_id) {
                    Ok(()) => {
                        let found = resolve(shared, &route).await;
                        match &found {
                            ResolveResult::Missing => log::info!(
                                "IMessage Bridge found no Agent session for inbound message `{message_id}`"
                            ),
                            ResolveResult::Found {
                                session,
                                open_required,
                            } => log::info!(
                                "IMessage Bridge resolved inbound message `{message_id}` to `{session}` (open_required={open_required})"
                            ),
                        }
                        Ok(found)
                    }
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            }
        }
        ToAgentRequest::Bind {
            route: route_request,
            message_id,
            session,
        } => {
            log::info!(
                "IMessage Bridge binding `{session}` to `{}` conversation `{}` for reply `{message_id}`",
                route_request.channel,
                route_request.conversation_id
            );
            match route(route_request) {
                Ok(route) => match bind(shared, route, &message_id, &session).await {
                    Ok(input_request) => {
                        match &input_request {
                            Some(request) => log::info!(
                                "IMessage Bridge routed `{message_id}` as the answer to `{session}` {request}"
                            ),
                            None => log::info!(
                                "IMessage Bridge queued reply `{message_id}` for `{session}`"
                            ),
                        }
                        return ToAgentResponse::Bound {
                            session,
                            input_request,
                        };
                    }
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            }
        }
    };
    match result {
        Ok(ResolveResult::Missing) => ToAgentResponse::Missing {},
        Ok(ResolveResult::Found {
            session,
            open_required,
        }) => ToAgentResponse::Resolved {
            open_required,
            session,
        },
        Err(error) => {
            log::warn!("IMessage Bridge rejected `to_agent`: {}", error.code());
            ToAgentResponse::Error { error }
        }
    }
}

pub(crate) struct ToAgentAction<Storage> {
    control: BridgeControl<Storage>,
}

impl<Storage> ToAgentAction<Storage> {
    pub(crate) const fn new(control: BridgeControl<Storage>) -> Self {
        Self { control }
    }
}

impl<Storage> WorkflowActionHandler for ToAgentAction<Storage>
where
    Storage: PluginStorage,
{
    type Request = ToAgentRequest;
    type Response = ToAgentResponse;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("imessage_bridge.to_agent");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let shared = Rc::clone(&self.control.shared);
        Box::pin(async move { Ok(invoke(&shared, request).await) })
    }
}

#[cfg(test)]
mod tests {
    use barracuda_imessage_gateway_plugin::{GatewayInboundMessage, GatewayRoute};
    use barracuda_workflow_plugin::{WorkflowActionSchema, workflow_action_schema};

    const TO_AGENT: WorkflowActionSchema = workflow_action_schema!("imessage_bridge.to_agent");

    #[test]
    fn threadless_gateway_event_matches_the_to_agent_resolve_schema() {
        let message = GatewayInboundMessage {
            route: GatewayRoute::new("web", "conversation"),
            message_id: "web-in-1".into(),
            text: "hello".into(),
        };

        assert!(
            serde_json::to_value(message)
                .is_ok_and(|input| TO_AGENT.request().validate(&input).is_ok())
        );
    }
}
