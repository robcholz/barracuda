#![allow(clippy::unwrap_used)]

use barracuda_agent_memory::{
    ProfileDocument, ProfileError, ProfileStore, DEFAULT_PROFILE_DOCUMENT_MAX_BYTES,
};
use barracuda_platform_test::memory_vfs;
use barracuda_vfs::ScopedVfs;
use futures_lite::future::block_on;

#[test]
fn missing_document_is_absent() {
    block_on(async {
        let store = store().await;
        assert_eq!(store.read(ProfileDocument::Soul).await.unwrap(), None);
    });
}

#[test]
fn replace_and_read_round_trip() {
    block_on(async {
        let store = store().await;
        store
            .replace(ProfileDocument::Soul, "Be concise.")
            .await
            .unwrap();
        assert_eq!(
            store.read(ProfileDocument::Soul).await.unwrap(),
            Some("Be concise.".to_string())
        );
    });
}

#[test]
fn clear_keeps_file_but_returns_empty_content() {
    block_on(async {
        let store = store().await;
        store
            .replace(ProfileDocument::UserProfile, "Use Chinese.")
            .await
            .unwrap();
        store.clear(ProfileDocument::UserProfile).await.unwrap();
        assert_eq!(
            store.read(ProfileDocument::UserProfile).await.unwrap(),
            Some(String::new())
        );
    });
}

#[test]
fn rejects_too_large_document() {
    block_on(async {
        let store = store().await;
        let content = "x".repeat(DEFAULT_PROFILE_DOCUMENT_MAX_BYTES + 1);
        let error = store
            .replace(ProfileDocument::AssistantIdentity, content)
            .await
            .unwrap_err();
        assert!(matches!(error, ProfileError::TooLarge { .. }));
    });
}

#[test]
fn invalid_utf8_is_an_error() {
    block_on(async {
        let (filesystem, store) = store_with_fs().await;
        filesystem
            .write_atomic("/memory/soul.md", &[0xff])
            .await
            .unwrap();
        let error = store.read(ProfileDocument::Soul).await.unwrap_err();
        assert!(matches!(error, ProfileError::InvalidUtf8 { .. }));
    });
}

#[test]
fn parses_document_ids() {
    assert_eq!("soul".parse(), Ok(ProfileDocument::Soul));
    assert_eq!("SOUL".parse(), Ok(ProfileDocument::Soul));
    assert_eq!("identity".parse(), Ok(ProfileDocument::AssistantIdentity));
    assert_eq!(
        "assistant_identity".parse(),
        Ok(ProfileDocument::AssistantIdentity)
    );
    assert_eq!("user".parse(), Ok(ProfileDocument::UserProfile));
    assert_eq!("user_profile".parse(), Ok(ProfileDocument::UserProfile));
}

#[test]
fn document_ids_use_canonical_labels() {
    let identity: &'static str = ProfileDocument::AssistantIdentity.into();
    let user: &'static str = ProfileDocument::UserProfile.into();
    assert_eq!(identity, "assistant_identity");
    assert_eq!(user, "user_profile");
    assert_eq!(
        ProfileDocument::AssistantIdentity.id(),
        "assistant_identity"
    );
    assert_eq!(ProfileDocument::UserProfile.to_string(), "user_profile");
}

async fn store() -> ProfileStore {
    store_with_fs().await.1
}

async fn store_with_fs() -> (ScopedVfs, ProfileStore) {
    let filesystem = memory_vfs().await.unwrap();
    let store = ProfileStore::new(filesystem.clone(), "/memory");
    (filesystem, store)
}
