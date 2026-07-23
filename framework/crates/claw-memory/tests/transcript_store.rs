#![allow(clippy::unwrap_used)]

use claw_interface::MemFs;
use claw_memory::{AssistantFinish, Transcript, TranscriptStore, TurnError, TurnHandle};

#[test]
fn streamed_drafts_are_visible_and_commit_on_drop() {
    let store = store();
    {
        let mut turn = store.open_turn().unwrap();
        turn.append_user("hel").unwrap();
        turn.append_user("lo").unwrap();
        // Before commit the open turn is the sole (trailing, id == None) entry.
        assert_eq!(store.turns()[0].messages[0]["content"], "hello");
        turn.finish_user().unwrap();

        turn.append_assistant("wo").unwrap();
        turn.append_assistant("rld").unwrap();
        assert_eq!(store.turns()[0].messages[1]["content"], "world");
    }

    let turns = store.turns();
    assert_eq!(turns.len(), 1);
    assert!(turns[0].id.is_some()); // committed turns carry a stable id
    assert_eq!(turns[0].messages[0]["content"], "hello");
    assert_eq!(turns[0].messages[1]["content"], "world");
}

#[test]
fn assistant_finish_replaces_streamed_draft_with_authoritative_json() {
    let store = store();
    let mut turn = store.open_turn().unwrap();
    turn.append_assistant("visible").unwrap();
    turn.finish_assistant(AssistantFinish::RawJson(
        r#"{"role":"assistant","content":"visible","reasoning_content":"hidden"}"#,
    ))
    .unwrap();

    let turns = store.turns();
    assert_eq!(turns[0].messages[0]["content"], "visible");
    assert_eq!(turns[0].messages[0]["reasoning_content"], "hidden");
}

#[test]
fn discard_drops_uncommitted_messages() {
    let store = store();
    let mut turn = store.open_turn().unwrap();
    turn.append_user("partial").unwrap();
    let before = store.version();

    turn.discard();

    assert!(store.version() > before);
    assert!(store.turns().is_empty());
}

#[test]
fn discard_keeps_committed_history() {
    let store = store();
    {
        let mut turn = store.open_turn().unwrap();
        turn.append_user("committed").unwrap();
    }

    let mut turn = store.open_turn().unwrap();
    turn.append_user("partial").unwrap();
    turn.discard();

    let turns = store.turns();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].messages[0]["content"], "committed");
}

#[test]
fn a_second_turn_cannot_open_until_the_handle_finishes() {
    let store = store();
    let turn = store.open_turn().unwrap();

    assert!(matches!(store.open_turn(), Err(TurnError::AlreadyOpen)));

    turn.discard();
    assert!(store.open_turn().is_ok());
}

#[test]
fn an_invalid_transition_poisons_and_discards_the_turn() {
    let store = store();
    let mut turn = store.open_turn().unwrap();
    turn.append_user("partial").unwrap();

    assert_eq!(
        turn.append_assistant("invalid"),
        Err(TurnError::AssistantWhileUserOpen)
    );
    assert_eq!(turn.commit(), Err(TurnError::Poisoned));
    assert!(store.turns().is_empty());
}

#[test]
fn tool_results_are_recorded_atomically() {
    let store = store();
    let mut turn = store.open_turn().unwrap();
    turn.record_tool_result("call-1", r#"{"temp_c":21}"#, false)
        .unwrap();
    turn.commit().unwrap();

    let turns = store.turns();
    assert_eq!(turns[0].messages[0]["role"], "tool");
    assert_eq!(turns[0].messages[0]["tool_call_id"], "call-1");
    assert_eq!(turns[0].messages[0]["is_error"], false);
}

#[test]
fn transcript_trait_erases_only_the_store_filesystem_type() {
    let store = store();
    let transcript: Box<dyn Transcript> = Box::new(store.clone());

    let mut turn: TurnHandle = transcript.open_turn().unwrap();
    turn.append_user("erased filesystem").unwrap();
    turn.commit().unwrap();

    assert_eq!(
        transcript.turns()[0].messages[0]["content"],
        "erased filesystem"
    );
}

fn store() -> TranscriptStore<MemFs> {
    MemFs::new();
    TranscriptStore::new(1, "/transcript-store-tests").unwrap()
}
