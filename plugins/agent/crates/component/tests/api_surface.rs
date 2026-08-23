#![allow(missing_docs)]

use barracuda_agent_component::delete_session::{
    delete_session_handler, DeleteSession, DeleteSessionError, DeleteSessionRequest,
};
use barracuda_agent_component::link_api::{
    link_api_handler, LinkApi, LinkApiError, LinkApiRequest,
};
use barracuda_agent_component::list_sessions::{
    list_sessions_handler, ListSessions, ListSessionsResponse,
};
use barracuda_agent_component::new_session::{
    new_session_handler, NewSession, NewSessionError, NewSessionRequest, NewSessionResponse,
};
use barracuda_agent_component::open_session::{
    open_session_handler, OpenSession, OpenSessionError, OpenSessionRequest,
    OpenSessionResponseFrame,
};
use barracuda_agent_component::session;
use barracuda_event_router::{
    MemFs, RpcInputMode, RpcMessage, RpcMethod, RpcOutputMode, Streaming, Unary,
};
use barracuda_net::testing::ScriptedStack;

fn assert_method<M, Request, Response, Error, Input, Output>()
where
    M: RpcMethod<
        Request = Request,
        Response = Response,
        Error = Error,
        Input = Input,
        Output = Output,
    >,
    Request: RpcMessage,
    Response: RpcMessage,
    Error: RpcMessage,
    Input: RpcInputMode<Request>,
    Output: RpcOutputMode<Response, Error>,
{
}

#[test]
fn runtime_rpcs_are_exposed_at_the_component_root() {
    let _ = link_api_handler::<MemFs, ScriptedStack>;
    let _ = new_session_handler::<MemFs, ScriptedStack>;
    let _ = list_sessions_handler::<MemFs, ScriptedStack>;
    let _ = open_session_handler::<MemFs, ScriptedStack>;
    let _ = delete_session_handler::<MemFs, ScriptedStack>;
    assert_eq!(LinkApi::ADDRESS, "agent.link_api");
    assert_method::<LinkApi, LinkApiRequest, (), LinkApiError, Unary, Unary>();

    assert_eq!(NewSession::ADDRESS, "agent.new_session");
    assert_method::<NewSession, NewSessionRequest, NewSessionResponse, NewSessionError, Unary, Unary>(
    );

    assert_eq!(ListSessions::ADDRESS, "agent.list_sessions");
    assert_method::<ListSessions, (), ListSessionsResponse, (), Unary, Streaming>();

    assert_eq!(OpenSession::ADDRESS, "agent.open_session");
    assert_method::<
        OpenSession,
        OpenSessionRequest,
        OpenSessionResponseFrame,
        OpenSessionError,
        Unary,
        Streaming,
    >();

    assert_eq!(DeleteSession::ADDRESS, "agent.delete_session");
    assert_method::<DeleteSession, DeleteSessionRequest, (), DeleteSessionError, Unary, Unary>();
}

#[test]
fn session_control_rpcs_live_in_the_session_module() {
    let _public_handlers = (
        session::append::append_handler,
        session::respond::respond_handler,
        session::set_reasoning_effort::set_reasoning_effort_handler,
        session::set_permission_level::set_permission_level_handler,
        session::interrupt::interrupt_handler,
        session::cancel::cancel_handler,
        session::close::close_handler,
    );
    assert_eq!(session::append::Append::ADDRESS, "session.append");
    assert_method::<
        session::append::Append,
        session::append::AppendRequestFrame,
        (),
        session::SessionRpcError,
        Streaming,
        Unary,
    >();

    assert_eq!(session::respond::Respond::ADDRESS, "session.respond");
    assert_method::<
        session::respond::Respond,
        session::respond::RespondRequestFrame,
        (),
        session::SessionRpcError,
        Streaming,
        Unary,
    >();

    assert_eq!(
        session::set_reasoning_effort::SetReasoningEffort::ADDRESS,
        "session.set_reasoning_effort"
    );
    assert_method::<
        session::set_reasoning_effort::SetReasoningEffort,
        session::set_reasoning_effort::SetReasoningEffortRequest,
        (),
        session::SessionRpcError,
        Unary,
        Unary,
    >();

    assert_eq!(
        session::set_permission_level::SetPermissionLevel::ADDRESS,
        "session.set_permission_level"
    );
    assert_method::<
        session::set_permission_level::SetPermissionLevel,
        session::set_permission_level::SetPermissionLevelRequest,
        (),
        session::SessionRpcError,
        Unary,
        Unary,
    >();

    assert_eq!(session::interrupt::Interrupt::ADDRESS, "session.interrupt");
    assert_method::<
        session::interrupt::Interrupt,
        session::interrupt::InterruptRequest,
        (),
        session::SessionRpcError,
        Unary,
        Unary,
    >();

    assert_eq!(session::cancel::Cancel::ADDRESS, "session.cancel");
    assert_method::<
        session::cancel::Cancel,
        session::cancel::CancelRequest,
        (),
        session::SessionRpcError,
        Unary,
        Unary,
    >();

    assert_eq!(session::close::Close::ADDRESS, "session.close");
    assert_method::<
        session::close::Close,
        session::close::CloseRequest,
        (),
        session::SessionRpcError,
        Unary,
        Unary,
    >();
}
