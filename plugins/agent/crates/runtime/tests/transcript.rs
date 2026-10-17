//! A session's transcript reads back what the person and the Agent said.

use barracuda_agent_runtime::{
    AgentRuntime, ApiPurpose, BackendKind, Message, ModelApiConfig, ModelApiFactory,
    RuntimeStorageConfig, SessionEvent, SessionPersistence, SessionTranscriptError, TranscriptRole,
    TurnEvent,
};
use barracuda_model_api::ModelApi;
use barracuda_platform_test::{memory_vfs, ScriptStep, ScriptedStack};
use futures_lite::future::{block_on, zip};
use futures_lite::StreamExt as _;
use http_client::ClientFactory;

fn reply(text: &str) -> String {
    format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}]}}\n\ndata: [DONE]\n\n")
}

#[test]
fn sessions_read_back_what_was_said() {
    // The runtime's model clients outlive this test's stack frame.
    let network: &'static ScriptedStack = Box::leak(Box::new(ScriptedStack::new([
        ScriptStep::sse(200, &[&reply("Hi there.")]),
        ScriptStep::sse(200, &[&reply("Noted.")]),
    ])));
    let llm_factory =
        ModelApiFactory::new(|| ModelApi::new(ClientFactory::from_network(network, network)));
    let filesystem = block_on(memory_vfs()).expect("memory VFS mounts");
    let (runtime, service) = AgentRuntime::new(
        filesystem,
        RuntimeStorageConfig {
            persistence_root: "/agent".into(),
            skill_roots: Vec::new(),
        },
        llm_factory,
    )
    .expect("runtime builds");
    runtime
        .set_api(
            ModelApiConfig::new(
                BackendKind::OpenAiCompatible,
                "key",
                "model",
                "http://llm.test/v1",
            ),
            ApiPurpose::RootAgent,
            true,
        )
        .expect("model configures");

    block_on(async {
        let scenario = async {
            for (persistence, said, answer) in [
                (SessionPersistence::Ephemeral, "Hello", "Hi there."),
                (SessionPersistence::Persistent, "Remember this", "Noted."),
            ] {
                let session = runtime
                    .new_session(persistence)
                    .await
                    .expect("session creates");
                assert!(runtime
                    .session_transcript(session, 8, 64)
                    .await
                    .expect("transcript")
                    .is_empty());
                let (control, mut events) = runtime.open_session(session).await.expect("opens");
                control
                    .append(Message::text(said))
                    .await
                    .expect("message appends");
                while let Some(event) = events.next().await {
                    if matches!(event, Ok(SessionEvent::Turn(TurnEvent::Ended { .. }))) {
                        break;
                    }
                }
                let transcript = runtime
                    .session_transcript(session, 8, 64)
                    .await
                    .expect("transcript");
                let read: Vec<_> = transcript
                    .iter()
                    .map(|entry| (entry.role, entry.text.as_str(), entry.open))
                    .collect();
                assert_eq!(
                    read,
                    [
                        (TranscriptRole::User, said, false),
                        (TranscriptRole::Assistant, answer, false)
                    ],
                    "{persistence:?}"
                );
                // The latest message alone, cut short.
                let last = runtime
                    .session_transcript(session, 1, 2)
                    .await
                    .expect("transcript");
                assert_eq!(last.len(), 1);
                assert_eq!(last[0].text, &answer[..2]);

                runtime.delete_session(session).await.expect("deletes");
                assert!(matches!(
                    runtime.session_transcript(session, 8, 64).await,
                    Err(SessionTranscriptError::SessionNotFound(_))
                ));
            }
            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}
