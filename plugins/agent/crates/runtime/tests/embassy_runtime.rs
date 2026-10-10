use std::rc::Rc;

use barracuda_agent_runtime::{
    AgentRuntime, Message, ModelApiFactory, RuntimeStorageConfig, SessionId, SessionInfo,
    SessionPersistence, SessionRenameError,
};
use barracuda_model_api::ModelApi;
use barracuda_platform_test::{memory_vfs, NeverStack};
use futures_lite::future::{block_on, zip};
use http_client::ClientFactory;

static NETWORK: NeverStack = NeverStack;

#[test]
fn executor_neutral_service_drives_public_session_api() {
    let llm_factory =
        ModelApiFactory::new(|| ModelApi::new(ClientFactory::from_network(&NETWORK, &NETWORK)));
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

    block_on(async move {
        let scenario = async {
            let session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            assert_eq!(runtime.list_sessions().await, vec![session]);
            runtime
                .delete_session(session)
                .await
                .expect("session deletes");
            assert!(runtime.list_sessions().await.is_empty());
            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}

#[test]
fn session_metadata_tracks_first_message_clock_and_rename() {
    let llm_factory =
        ModelApiFactory::new(|| ModelApi::new(ClientFactory::from_network(&NETWORK, &NETWORK)));
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
    // Installed before the service starts; it must still reach the session.
    runtime.set_wall_clock(Rc::new(|| Some(1_700_000_000_000)));

    block_on(async {
        let scenario = async {
            let session = runtime
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            assert_eq!(
                runtime.describe_sessions().await,
                vec![SessionInfo {
                    session,
                    persistence: SessionPersistence::Ephemeral,
                    title: None,
                    updated_at: None,
                }]
            );

            let (control, _events) = runtime.open_session(session).await.expect("session opens");
            control
                .append(Message::text("\n  Plan the trip  \nwith details"))
                .await
                .expect("message appends");
            let described = runtime.describe_sessions().await;
            assert_eq!(described.len(), 1);
            assert_eq!(described[0].title.as_deref(), Some("Plan the trip"));
            assert_eq!(described[0].updated_at, Some(1_700_000_000_000));

            runtime
                .rename_session(session, "  Renamed  ")
                .await
                .expect("session renames");
            assert_eq!(
                runtime.rename_session(session, "   ").await,
                Err(SessionRenameError::InvalidTitle)
            );
            assert_eq!(
                runtime.rename_session(SessionId::new(99), "title").await,
                Err(SessionRenameError::SessionNotFound(SessionId::new(99)))
            );
            let described = runtime.describe_sessions().await;
            assert_eq!(described[0].title.as_deref(), Some("Renamed"));
            runtime.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
        assert_eq!(
            runtime.rename_session(SessionId::new(1), "late").await,
            Err(SessionRenameError::WorkerStopped)
        );
        assert!(runtime.describe_sessions().await.is_empty());
    });
}
