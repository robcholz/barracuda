#![allow(clippy::expect_used)]

use barracuda_agent::SharedApiManager;
use barracuda_agent_persistence::Persistence;
use barracuda_agent_session::{
    SessionId, SessionInfo, SessionManager, SessionPersistence, SessionRenameError,
};
use barracuda_agent_tool::ToolRegistry;
use barracuda_model_api::{ModelApi, ModelApiFactory};
use barracuda_platform_test::{memory_vfs, NeverStack};
use futures_lite::future::block_on;
use http_client::ClientFactory;
use portable_atomic_util::Arc;

static NETWORK: NeverStack = NeverStack;

fn manager() -> SessionManager<NeverStack, NeverStack> {
    block_on(async {
        let filesystem = memory_vfs().await.expect("memory VFS mounts");
        let persistence = Arc::new(
            Persistence::new(filesystem.clone(), "/agent")
                .await
                .expect("persistence opens"),
        );
        let tools = Arc::new(
            ToolRegistry::new(Arc::clone(&persistence))
                .await
                .expect("Tool Registry loads"),
        );
        let llm_factory =
            ModelApiFactory::new(|| ModelApi::new(ClientFactory::from_network(&NETWORK, &NETWORK)));
        SessionManager::new(
            filesystem,
            tools,
            persistence,
            "/agent".into(),
            Vec::new(),
            SharedApiManager::default(),
            llm_factory,
        )
        .await
        .expect("session manager loads")
    })
}

#[test]
fn describe_reports_every_live_session_sorted_by_id() {
    let mut manager = manager();
    let persistent = manager
        .create(SessionPersistence::Persistent)
        .expect("persistent session creates");
    let ephemeral = manager
        .create(SessionPersistence::Ephemeral)
        .expect("ephemeral session creates");

    assert_eq!(
        manager.describe(),
        vec![
            SessionInfo {
                session: persistent,
                persistence: SessionPersistence::Persistent,
                title: None,
                updated_at: None,
            },
            SessionInfo {
                session: ephemeral,
                persistence: SessionPersistence::Ephemeral,
                title: None,
                updated_at: None,
            },
        ]
    );
}

#[test]
fn rename_normalizes_the_title() {
    let mut manager = manager();
    let session = manager
        .create(SessionPersistence::Ephemeral)
        .expect("session creates");

    manager
        .rename(session, "\n   Weekend plans  \nignored")
        .expect("session renames");
    let long = "x".repeat(80);
    let other = manager
        .create(SessionPersistence::Persistent)
        .expect("session creates");
    manager.rename(other, &long).expect("session renames");

    let titles: Vec<_> = manager
        .describe()
        .into_iter()
        .map(|info| info.title)
        .collect();
    assert_eq!(
        titles,
        vec![
            Some("Weekend plans".to_owned()),
            Some(format!("{}\u{2026}", "x".repeat(63))),
        ]
    );
}

#[test]
fn rename_rejects_blank_titles_and_unknown_sessions() {
    let mut manager = manager();
    let session = manager
        .create(SessionPersistence::Ephemeral)
        .expect("session creates");
    manager.rename(session, "kept").expect("session renames");

    assert_eq!(
        manager.rename(session, " \n\t "),
        Err(SessionRenameError::InvalidTitle)
    );
    assert_eq!(
        manager.rename(SessionId::new(99), "title"),
        Err(SessionRenameError::SessionNotFound(SessionId::new(99)))
    );
    assert_eq!(
        manager
            .describe()
            .first()
            .and_then(|info| info.title.clone()),
        Some("kept".to_owned())
    );
}
