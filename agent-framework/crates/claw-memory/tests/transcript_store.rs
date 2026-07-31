#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use claw_interface::MemFs;
use claw_memory::{AssistantFragment, Transcript, TranscriptStore, TurnError, TurnHandle, TurnId};

#[test]
fn turn_id_uses_the_shared_prefixed_wire_format() {
    let id = TurnId::new(7);

    assert_eq!(id.to_string(), "turn-7");
    assert_eq!(serde_json::to_value(id).unwrap(), "turn-7");
    assert_eq!(
        serde_json::from_value::<TurnId>("turn-7".into()).unwrap(),
        id
    );
}

#[test]
fn message_handles_expose_drafts_and_finish_on_drop() {
    let store = store();
    let turn = store.clone().open_turn().unwrap();

    {
        let mut user = turn.user().unwrap();
        user.append("hel");
        user.append("lo");
        assert_eq!(store.turns()[0].messages[0]["content"], "hello");
    }

    {
        let mut assistant = turn.assistant().unwrap();
        assistant.append(AssistantFragment::Content("wo"));
        assistant.append(AssistantFragment::Content("rld"));
        assert_eq!(store.turns()[0].messages[1]["content"], "world");
    }

    drop(turn);
    let turns = store.turns();
    assert_eq!(turns.len(), 1);
    assert!(turns[0].id.is_some());
    assert_eq!(turns[0].messages[0]["content"], "hello");
    assert_eq!(turns[0].messages[1]["content"], "world");
}

#[test]
fn assistant_handle_builds_structured_message() {
    let store = store();
    let turn = store.clone().open_turn().unwrap();
    {
        let mut assistant = turn.assistant().unwrap();
        assistant.append(AssistantFragment::Content("visible"));
        assistant.append(AssistantFragment::Reasoning("hidden"));
        assistant.append(AssistantFragment::ToolCall(
            serde_json::json!({"id": "call-1"}),
        ));
    }

    let message = &store.turns()[0].messages[0];
    assert_eq!(message["content"], "visible");
    assert_eq!(message["reasoning_content"], "hidden");
    assert_eq!(message["tool_calls"][0]["id"], "call-1");
}

#[test]
fn turn_version_advances_only_when_the_turn_drops() {
    let store = store();
    let initial_turn_version = store.turn_version();
    let turn = store.clone().open_turn().unwrap();

    {
        let mut user = turn.user().unwrap();
        user.append("hello");
    }
    assert_eq!(store.turn_version(), initial_turn_version);

    {
        let mut assistant = turn.assistant().unwrap();
        assistant.append(AssistantFragment::Content("world"));
    }
    assert_eq!(store.turn_version(), initial_turn_version);

    drop(turn);
    assert_eq!(store.turn_version(), initial_turn_version.saturating_add(1));
}

#[test]
fn an_empty_turn_does_not_advance_turn_version() {
    let store = store();
    let initial_turn_version = store.turn_version();

    drop(store.clone().open_turn().unwrap());

    assert_eq!(store.turn_version(), initial_turn_version);
    assert!(store.turns().is_empty());
}

#[test]
fn a_second_turn_cannot_open_until_the_handle_drops() {
    let store = store();
    let turn = store.clone().open_turn().unwrap();

    assert!(matches!(
        store.clone().open_turn(),
        Err(TurnError::AlreadyOpen)
    ));

    drop(turn);
    assert!(store.clone().open_turn().is_ok());
}

#[test]
fn a_second_message_cannot_open_until_the_child_handle_drops() {
    let store = store();
    let turn = store.open_turn().unwrap();
    let user = turn.user().unwrap();

    assert!(matches!(
        turn.assistant(),
        Err(TurnError::MessageAlreadyOpen)
    ));

    drop(user);
    assert!(turn.assistant().is_ok());
}

#[test]
fn tool_handle_records_one_atomic_result() {
    let store = store();
    let turn = store.clone().open_turn().unwrap();
    {
        let mut tool = turn.tool("call-1", false).unwrap();
        tool.append(r#"{"temp_"#);
        tool.append(r#"c":21}"#);
    }

    let message = &store.turns()[0].messages[0];
    assert_eq!(message["role"], "tool");
    assert_eq!(message["tool_call_id"], "call-1");
    assert_eq!(message["content"], r#"{"temp_c":21}"#);
    assert_eq!(message["is_error"], false);
}

#[test]
fn turn_drop_can_persist_after_the_store_drops() {
    let filesystem = Arc::new(MemFs::new());
    let store =
        TranscriptStore::new(Arc::clone(&filesystem), 9, "/transcript-detached-turn").unwrap();
    let turn = store.open_turn().unwrap();
    {
        let mut user = turn.user().unwrap();
        user.append("still persists");
    }

    drop(store);
    drop(turn);

    let reloaded = TranscriptStore::new(filesystem, 9, "/transcript-detached-turn").unwrap();
    assert_eq!(reloaded.turns()[0].messages[0]["content"], "still persists");
}

#[test]
fn transcript_trait_is_the_only_type_erased_boundary() {
    let transcript: Arc<dyn Transcript> = store();

    let turn: TurnHandle = transcript.clone().open_turn().unwrap();
    {
        let mut user = turn.user().unwrap();
        user.append("erased filesystem");
    }
    drop(turn);

    assert_eq!(
        transcript.turns()[0].messages[0]["content"],
        "erased filesystem"
    );
    assert_eq!(transcript.turn_version(), 1);
}

#[test]
fn persisted_transcript_restores_turn_version() {
    let filesystem = Arc::new(MemFs::new());
    let store = Arc::new(
        TranscriptStore::<MemFs>::new(Arc::clone(&filesystem), 7, "/transcript-version-reload")
            .unwrap(),
    );
    {
        let turn = store.clone().open_turn().unwrap();
        {
            let mut user = turn.user().unwrap();
            user.append("hello");
        }
        {
            let mut assistant = turn.assistant().unwrap();
            assistant.append(AssistantFragment::Content("world"));
        }
    }
    assert_eq!(store.turn_version(), 1);

    let reloaded =
        TranscriptStore::<MemFs>::new(filesystem, 7, "/transcript-version-reload").unwrap();
    assert_eq!(reloaded.turn_version(), 1);
}

fn store() -> Arc<TranscriptStore<MemFs>> {
    Arc::new(TranscriptStore::new(Arc::new(MemFs::new()), 1, "/transcript-store-tests").unwrap())
}
