use claw_agent::{AgentPersistenceConfig, AgentSystem, ClawApiFactory, SessionPersistence};
use claw_api::ClawApi;
use claw_interface::MemFs;
use claw_net::testing::NeverStack;
use futures_lite::future::{block_on, zip};

static NETWORK: NeverStack = NeverStack;

#[test]
fn executor_neutral_service_drives_public_session_api() {
    let llm_factory = ClawApiFactory::new(|| ClawApi::new(&NETWORK, 1024, 1024));
    let (system, service) = AgentSystem::<MemFs, NeverStack>::new(
        MemFs::new(),
        AgentPersistenceConfig {
            persistence_root: "/agent".into(),
            skill_roots: Vec::new(),
        },
        llm_factory,
    )
    .expect("runtime builds");

    block_on(async move {
        let scenario = async {
            let session = system
                .new_session(SessionPersistence::Ephemeral)
                .await
                .expect("session creates");
            assert_eq!(system.list_sessions().await, vec![session]);
            system
                .delete_session(session)
                .await
                .expect("session deletes");
            assert!(system.list_sessions().await.is_empty());
            system.shutdown().await;
        };
        let (_, ()) = zip(service, scenario).await;
    });
}
