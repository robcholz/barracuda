use barracuda_agent_runtime::{
    AgentRuntime, ModelApiFactory, RuntimeStorageConfig, SessionPersistence,
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
