//! Data-driven tests for the skill registry over real `SKILL.md` fixtures.
//!
//! Run with `BARRACUDA_UPDATE_GOLDEN=1` to regenerate the golden files.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arc_with_non_send_sync
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use barracuda_agent_skill::{FsSkillRegistry, SkillName};
use barracuda_platform_test::memory_vfs;
use futures_lite::future::block_on;
use serde_json::Value;

const SKILLS_ROOT: &str = "skills";

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

fn expected_dir() -> PathBuf {
    data_dir().join("skills_expected")
}

fn update_golden() -> bool {
    std::env::var_os("BARRACUDA_UPDATE_GOLDEN").is_some()
}

async fn registry() -> Arc<FsSkillRegistry> {
    let filesystem = memory_vfs().await.expect("mount memory VFS");
    for (path, contents) in fixture_files() {
        filesystem
            .write_atomic(&path, &contents)
            .await
            .expect("copy skill fixture into VFS");
    }
    Arc::new(
        FsSkillRegistry::new(filesystem)
            .add_root(SKILLS_ROOT)
            .await
            .expect("scan skills fixtures"),
    )
}

fn fixture_files() -> Vec<(String, Vec<u8>)> {
    fn collect(directory: &Path, relative: &Path, files: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(directory).expect("read fixture directory") {
            let entry = entry.expect("read fixture entry");
            let path = entry.path();
            let relative = relative.join(entry.file_name());
            if path.is_dir() {
                collect(&path, &relative, files);
            } else {
                files.push((
                    format!("/{SKILLS_ROOT}/{}", relative.display()),
                    std::fs::read(path).expect("read skill fixture"),
                ));
            }
        }
    }

    let mut files = Vec::new();
    collect(&data_dir().join(SKILLS_ROOT), Path::new(""), &mut files);
    files
}

fn catalog_json(registry: &Arc<FsSkillRegistry>) -> String {
    let mut set = registry.skill_set();
    let mut rendered = set.list_skills().to_string();
    rendered.push('\n');
    rendered
}

fn catalog_ids(catalog: &str) -> Vec<String> {
    let value: Value = serde_json::from_str(catalog).expect("catalog json");
    value
        .as_array()
        .expect("catalog array")
        .iter()
        .map(|entry| {
            entry
                .get("name")
                .and_then(Value::as_str)
                .expect("catalog id")
                .to_string()
        })
        .collect()
}

fn assert_golden(path: &Path, actual: &str, label: &str) {
    if update_golden() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create golden dir");
        }
        std::fs::write(path, actual).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(path).unwrap_or_else(|_| {
        panic!(
            "missing golden for {label}: {} - run with BARRACUDA_UPDATE_GOLDEN=1 to generate",
            path.display()
        )
    });
    assert_eq!(
        actual,
        &expected,
        "{label} does not match golden {}",
        path.display()
    );
}

#[test]
fn catalog_matches_golden() {
    block_on(async {
        let registry = registry().await;
        let catalog = catalog_json(&registry);
        assert_ne!(catalog, "[]\n", "no skills scanned from tests/data/skills");
        assert_golden(&expected_dir().join("catalog.json"), &catalog, "catalog");
    });
}

#[test]
fn documents_match_golden() {
    block_on(async {
        let registry = registry().await;
        let catalog = catalog_json(&registry);
        let mut set = registry.skill_set();
        for id in catalog_ids(&catalog) {
            let document = set
                .read_skill(&SkillName::new(id.clone()))
                .await
                .expect("read skill document");
            assert!(
                !document.content().contains("\n---\n"),
                "front-matter not stripped for {id}"
            );
            assert_golden(
                &expected_dir().join(&id).join("document.md"),
                document.content(),
                &format!("document for {id}"),
            );
        }
    });
}

#[test]
fn skill_set_reads_fixture_documents() {
    block_on(async {
        let registry = registry().await;
        let catalog = catalog_json(&registry);
        let first = catalog_ids(&catalog)
            .into_iter()
            .next()
            .expect("at least one fixture skill");

        let mut set = registry.skill_set();
        let document = set
            .read_skill(&SkillName::new(first.clone()))
            .await
            .expect("read skill");
        assert!(
            !document.content().is_empty(),
            "skill instructions are empty for {first}"
        );
    });
}

#[test]
fn reading_unknown_skill_is_not_found() {
    block_on(async {
        let registry = registry().await;
        let mut set = registry.skill_set();
        assert!(set
            .read_skill(&SkillName::new("does-not-exist"))
            .await
            .is_err());
    });
}
