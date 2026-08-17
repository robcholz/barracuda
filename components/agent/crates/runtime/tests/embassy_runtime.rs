use barracuda_agent_runtime::{
    AgentRuntime, ModelApiFactory, RuntimeStorageConfig, SessionPersistence,
};
use barracuda_fs::MemFs;
use barracuda_model_api::ModelApi;
use barracuda_net::testing::NeverStack;
use futures_lite::future::{block_on, zip};

static NETWORK: NeverStack = NeverStack;

#[test]
fn executor_neutral_service_drives_public_session_api() {
    let llm_factory = ModelApiFactory::new(|| ModelApi::new(&NETWORK, 1024, 1024));
    let (runtime, service) = AgentRuntime::<MemFs, NeverStack>::new(
        MemFs::new(),
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
