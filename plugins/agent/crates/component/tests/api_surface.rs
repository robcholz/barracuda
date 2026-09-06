#![allow(missing_docs)]

use barracuda_agent_component::{
    delete_session::DeleteSession,
    list_sessions::ListSessions,
    new_session::NewSession,
    open_session::OpenSession,
    session::{self, SessionOutputEvent},
};
use barracuda_event_router::{Event, JsonRpcSchema};

fn assert_json_method<Method: JsonRpcSchema>(address: &str) {
    assert_eq!(Method::ADDRESS, address);
    assert!(Method::MAX_REQUEST_BYTES > 0);
    assert!(Method::MAX_REQUEST_BYTES <= 512);
    assert!(Method::MAX_RESPONSE_BYTES > 0);
    assert!(Method::MAX_RESPONSE_BYTES <= 512);
    assert!(Method::REQUEST_SCHEMA.as_str().contains("\"type\""));
    assert!(Method::RESPONSE_SCHEMA.as_str().contains("\"type\""));
}

#[test]
fn every_agent_operation_is_a_bounded_json_contract() {
    assert_json_method::<NewSession>("session.new");
    assert_json_method::<ListSessions>("session.list");
    assert_json_method::<OpenSession>("session.open");
    assert_json_method::<DeleteSession>("session.delete");
    assert_json_method::<session::append::Append>("session.append");
    assert_json_method::<session::respond::Respond>("session.respond");
    assert_json_method::<session::set_reasoning_effort::SetReasoningEffort>(
        "session.set_reasoning_effort",
    );
    assert_json_method::<session::set_permission_level::SetPermissionLevel>(
        "session.set_permission_level",
    );
    assert_json_method::<session::interrupt::Interrupt>("session.interrupt");
    assert_json_method::<session::cancel::Cancel>("session.cancel");
    assert_json_method::<session::close::Close>("session.close");
}

#[test]
fn open_session_streams_through_the_bounded_event_contract() {
    assert_eq!(SessionOutputEvent::ID, "session.event");
    let schema = include_str!("../../../schemas/event/session_event.json");
    for field in ["session", "sequence", "type", "payload"] {
        assert!(schema.contains(&format!("\"{field}\"")));
    }
    for removed in [
        "run",
        "chunk_index",
        "field",
        "chunk",
        "field_complete",
        "event_complete",
        "terminal",
    ] {
        assert!(!schema.contains(&format!("\"{removed}\"")));
    }
    assert!(!schema.contains("maxLength"));
}

#[test]
fn message_text_uses_the_complete_request_lane() {
    for schema in [
        include_str!("../../../schemas/rpc/append/request.json"),
        include_str!("../../../schemas/rpc/respond/request.json"),
    ] {
        assert!(!schema.contains("maxLength"));
    }
}
