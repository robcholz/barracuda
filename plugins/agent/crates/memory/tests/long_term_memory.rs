#![allow(clippy::unwrap_used)]

use barracuda_agent_memory::{
    LongTermError, LongTermMemory, MemoryDraft, MemoryId, MemoryPatch, StoreOutcome,
};
use barracuda_platform_test::memory_vfs;
use futures_lite::future::block_on;

#[test]
fn store_mints_prefixed_ids_in_order() {
    block_on(async {
        let memory = memory().await;
        let first = memory.store(draft("Likes tea", &["preference"])).await;
        let second = memory.store(draft("Lives in Berlin", &["fact"])).await;
        assert_eq!(first.item().id.as_str(), "g-0");
        assert_eq!(second.item().id.as_str(), "g-1");
    });
}

#[test]
fn store_dedups_by_normalized_content() {
    block_on(async {
        let memory = memory().await;
        assert!(matches!(
            memory.store(draft("Likes  TEA", &["preference"])).await,
            StoreOutcome::Created(_)
        ));
        match memory.store(draft("likes tea", &["preference"])).await {
            StoreOutcome::Duplicate(item) => assert_eq!(item.id.as_str(), "g-0"),
            other => panic!("expected duplicate, got {other:?}"),
        }
        assert_eq!(memory.list().len(), 1);
    });
}

#[test]
fn recall_filters_by_label_and_query_newest_first() {
    block_on(async {
        let memory = memory().await;
        memory.store(draft("Likes tea", &["preference"])).await;
        memory
            .store(draft("Likes coffee too", &["preference"]))
            .await;
        memory.store(draft("Has a dog", &["fact"])).await;

        let prefs = memory.recall(&["preference".to_string()], None, 10);
        assert_eq!(prefs.len(), 2);
        assert_eq!(prefs[0].content, "Likes coffee too");

        let tea = memory.recall(&["preference".to_string()], Some("tea"), 10);
        assert_eq!(tea.len(), 1);
        assert_eq!(tea[0].content, "Likes tea");

        assert_eq!(memory.recall(&[], None, 2).len(), 2);
    });
}

#[test]
fn recall_labels_ignore_ascii_case_and_duplicates_are_findable() {
    block_on(async {
        let memory = memory().await;
        memory.store(draft("Likes tea", &["Preference"])).await;

        assert_eq!(
            memory.recall(&["preference".to_string()], None, 10).len(),
            1
        );
        assert_eq!(
            memory.recall(&["PREFERENCE".to_string()], None, 10).len(),
            1
        );
        assert_eq!(
            memory
                .find_duplicate("  likes   TEA ")
                .map(|item| item.content),
            Some("Likes tea".to_string())
        );
        assert!(memory.find_duplicate("Likes coffee").is_none());
    });
}

#[test]
fn update_replaces_only_supplied_fields() {
    block_on(async {
        let memory = memory().await;
        let id = memory.store(draft("Old", &["x"])).await.item().id.clone();
        let updated = memory
            .update(
                &id,
                MemoryPatch {
                    content: Some("New".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("update");
        assert_eq!(updated.content, "New");
        assert_eq!(updated.tags, vec!["x".to_string()]);
        assert!(matches!(
            memory
                .update(&MemoryId::from("g-999"), MemoryPatch::default())
                .await,
            Err(LongTermError::NotFound(_))
        ));
    });
}

#[test]
fn forget_removes_the_item() {
    block_on(async {
        let memory = memory().await;
        let id = memory
            .store(draft("Ephemeral", &["x"]))
            .await
            .item()
            .id
            .clone();
        memory.forget(&id).await.expect("forget");
        assert!(memory.list().is_empty());
        assert!(matches!(
            memory.forget(&id).await,
            Err(LongTermError::NotFound(_))
        ));
    });
}

#[test]
fn state_survives_reload_from_journal() {
    block_on(async {
        let filesystem = memory_vfs().await.unwrap();
        {
            let memory = LongTermMemory::new(filesystem.clone(), "/m", "g-")
                .await
                .expect("load empty store");
            memory.store(draft("Persistent", &["fact"])).await;
            let id = memory
                .store(draft("To be edited", &["fact"]))
                .await
                .item()
                .id
                .clone();
            memory
                .update(
                    &id,
                    MemoryPatch {
                        content: Some("Edited".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .expect("update");
        }
        let reloaded = LongTermMemory::new(filesystem.clone(), "/m", "g-")
            .await
            .expect("replay journal");
        assert_eq!(reloaded.list().len(), 2);
        let edited = reloaded.recall(&[], Some("edited"), 10);
        assert_eq!(edited.len(), 1);
        assert_eq!(edited[0].content, "Edited");
        assert_eq!(
            reloaded
                .store(draft("Another", &["fact"]))
                .await
                .item()
                .id
                .as_str(),
            "g-2"
        );
    });
}

#[test]
fn torn_trailing_journal_record_is_ignored_on_reload() {
    block_on(async {
        let filesystem = memory_vfs().await.unwrap();
        {
            let memory = LongTermMemory::new(filesystem.clone(), "/m", "g-")
                .await
                .expect("load empty store");
            memory
                .store(draft("Committed before crash", &["fact"]))
                .await;
        }
        filesystem
            .append("/m/memory_records.jsonl", br#"{"torn":"record""#)
            .await
            .unwrap();

        let reloaded = LongTermMemory::new(filesystem.clone(), "/m", "g-")
            .await
            .expect("replay journal");
        let items = reloaded.list();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].content, "Committed before crash");
        assert_eq!(
            reloaded
                .store(draft("After crash", &["fact"]))
                .await
                .item()
                .id
                .as_str(),
            "g-1"
        );
    });
}

async fn memory() -> LongTermMemory {
    LongTermMemory::new(memory_vfs().await.unwrap(), "/m", "g-")
        .await
        .expect("load empty store")
}

fn draft(content: &str, tags: &[&str]) -> MemoryDraft {
    MemoryDraft::new(content).with_tags(tags.iter().map(|tag| (*tag).to_string()))
}
