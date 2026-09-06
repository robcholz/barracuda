#![allow(clippy::unwrap_used, clippy::arc_with_non_send_sync)]

use std::{future::Future, pin::Pin, sync::Arc};

use barracuda_agent_skill::{
    CatalogSnapshot, FsSkillRegistry, Skill, SkillError, SkillName, SkillRegistry,
};
use barracuda_platform_test::memory_vfs;
use barracuda_vfs::ScopedVfs;
use futures_lite::future::block_on;

struct ExternalRegistry {
    catalog: Arc<CatalogSnapshot>,
}

impl SkillRegistry for ExternalRegistry {
    fn catalog(&self) -> Arc<CatalogSnapshot> {
        Arc::clone(&self.catalog)
    }

    fn reload(&self) -> Pin<Box<dyn Future<Output = Result<(), SkillError>> + '_>> {
        Box::pin(async { Ok(()) })
    }

    fn read_document<'a>(
        &'a self,
        name: &'a SkillName,
    ) -> Pin<Box<dyn Future<Output = Result<String, SkillError>> + 'a>> {
        Box::pin(async move {
            if self.catalog.get(name).is_none() {
                return Err(SkillError::NotFound(name.clone()));
            }
            Ok("body".to_owned())
        })
    }
}

#[test]
fn public_registry_trait_drives_skill_set() {
    block_on(async {
        let skill = Skill::new(
            SkillName::new("external"),
            "External backend. Use for external operations.".to_owned(),
        )
        .unwrap();
        let registry: Arc<dyn SkillRegistry> = Arc::new(ExternalRegistry {
            catalog: Arc::new(CatalogSnapshot::from_skills(1, vec![skill])),
        });
        let mut skills = registry.skill_set();

        assert!(skills.catalog_context().contains("External backend"));
        assert_eq!(
            skills
                .read_skill(&SkillName::new("external"))
                .await
                .unwrap()
                .content(),
            "body"
        );
    });
}

#[test]
fn registry_parses_standard_frontmatter() {
    block_on(async {
        let filesystem = memory_vfs().await.unwrap();
        write_skill(
        &filesystem,
        "example-skill",
        "---\nname: example-skill\ndescription: >\n  Does a useful thing.\n  Use for examples.\nlicense: Apache-2.0\ncompatibility: Requires network access\nmetadata:\n  author: example-org\n  version: \"1.0\"\nallowed-tools: Read Bash(git:*)\n---\n# Instructions\n\nRead references/GUIDE.md.\n",
    ).await;

        let registry = Arc::new(
            FsSkillRegistry::new(filesystem)
                .set_root("skills")
                .await
                .unwrap(),
        );
        let catalog = registry.catalog();
        let skill = catalog.get(&SkillName::new("example-skill")).unwrap();

        assert_eq!(skill.name().as_str(), "example-skill");
        assert_eq!(skill.license(), Some("Apache-2.0"));
        assert_eq!(skill.compatibility(), Some("Requires network access"));
        assert_eq!(
            skill.metadata().get("author").map(String::as_str),
            Some("example-org")
        );
        assert_eq!(skill.allowed_tools(), Some("Read Bash(git:*)"));
        assert_eq!(skill.directory(), Some("skills/example-skill"));

        let mut skills = registry.skill_set();
        let list: serde_json::Value = serde_json::from_str(skills.list_skills()).unwrap();
        assert_eq!(
            list,
            serde_json::json!([{
                "name": "example-skill",
                "description": "Does a useful thing. Use for examples.\n"
            }])
        );
        let document = skills
            .read_skill(&SkillName::new("example-skill"))
            .await
            .unwrap();
        assert_eq!(
            document.content(),
            "# Instructions\n\nRead references/GUIDE.md."
        );
        assert_eq!(document.directory(), Some("skills/example-skill"));
    });
}

#[test]
fn unknown_frontmatter_subtrees_are_ignored() {
    block_on(async {
        let filesystem = memory_vfs().await.unwrap();
        write_skill(
            &filesystem,
            "example-skill",
            "---\nname: example-skill\nfuture-field:\n  nested:\n    - unsupported\n    - syntax\ndescription: Use for examples.\n---\nbody",
        )
        .await;

        let registry = FsSkillRegistry::new(filesystem)
            .set_root("skills")
            .await
            .unwrap();
        assert!(registry
            .catalog()
            .get(&SkillName::new("example-skill"))
            .is_some());
    });
}

#[test]
fn known_frontmatter_fields_reject_unsupported_yaml() {
    block_on(async {
        let filesystem = memory_vfs().await.unwrap();
        write_skill(
            &filesystem,
            "example-skill",
            "---\nname: example-skill\ndescription: [not, supported]\n---\nbody",
        )
        .await;

        assert!(matches!(
            registry_error(filesystem).await,
            SkillError::InvalidYaml(_, _)
        ));
    });
}

#[test]
fn duplicate_known_frontmatter_fields_are_rejected() {
    block_on(async {
        let filesystem = memory_vfs().await.unwrap();
        write_skill(
            &filesystem,
            "example-skill",
            "---\nname: example-skill\nname: example-skill\ndescription: Use for examples.\n---\nbody",
        )
        .await;

        assert!(matches!(
            registry_error(filesystem).await,
            SkillError::InvalidYaml(_, _)
        ));
    });
}

#[test]
fn json_frontmatter_is_not_accepted_as_legacy_format() {
    block_on(async {
        let filesystem = memory_vfs().await.unwrap();
        write_skill(
            &filesystem,
            "example-skill",
            "---\n{\"name\":\"example-skill\",\"description\":\"legacy\"}\n---\nbody",
        )
        .await;

        assert!(matches!(
            registry_error(filesystem).await,
            SkillError::InvalidYaml(_, _)
        ));
    });
}

#[test]
fn legacy_metadata_shapes_are_rejected() {
    block_on(async {
        let filesystem = memory_vfs().await.unwrap();
        write_skill(
        &filesystem,
        "example-skill",
        "---\nname: example-skill\ndescription: Use for examples.\nmetadata:\n  cap_groups:\n    - cap_lua\n  manage_mode: readonly\n---\nbody",
    ).await;

        assert!(matches!(
            registry_error(filesystem).await,
            SkillError::InvalidYaml(_, _)
        ));
    });
}

#[test]
fn missing_opening_fence_errors() {
    block_on(async {
        let filesystem = memory_vfs().await.unwrap();
        write_skill(&filesystem, "example-skill", "no frontmatter").await;
        assert!(matches!(
            registry_error(filesystem).await,
            SkillError::MissingOpeningFence(_)
        ));
    });
}

#[test]
fn missing_closing_fence_errors() {
    block_on(async {
        let filesystem = memory_vfs().await.unwrap();
        write_skill(
            &filesystem,
            "example-skill",
            "---\nname: example-skill\ndescription: Use for examples.\n",
        )
        .await;
        assert!(matches!(
            registry_error(filesystem).await,
            SkillError::MissingClosingFence(_)
        ));
    });
}

#[test]
fn name_must_match_directory() {
    block_on(async {
        let filesystem = memory_vfs().await.unwrap();
        write_skill(&filesystem, "example-skill", &skill_md("other-skill")).await;
        assert!(matches!(
            registry_error(filesystem).await,
            SkillError::InvalidFrontmatter(_, _)
        ));
    });
}

#[test]
fn name_rejects_legacy_underscores() {
    block_on(async {
        let filesystem = memory_vfs().await.unwrap();
        write_skill(&filesystem, "legacy_skill", &skill_md("legacy_skill")).await;
        assert!(matches!(
            registry_error(filesystem).await,
            SkillError::InvalidFrontmatter(_, _)
        ));
    });
}

#[test]
fn closing_fence_must_occupy_its_own_line() {
    block_on(async {
        let filesystem = memory_vfs().await.unwrap();
        write_skill(
            &filesystem,
            "example-skill",
            "---\nname: example-skill\ndescription: Use for examples.\n---not-a-fence\nbody",
        )
        .await;
        assert!(matches!(
            registry_error(filesystem).await,
            SkillError::MissingClosingFence(_)
        ));
    });
}

async fn write_skill(filesystem: &ScopedVfs, name: &str, document: &str) {
    filesystem
        .write_atomic(&format!("skills/{name}/SKILL.md"), document.as_bytes())
        .await
        .unwrap();
}

async fn registry_error(filesystem: ScopedVfs) -> SkillError {
    match FsSkillRegistry::new(filesystem).set_root("skills").await {
        Ok(_) => panic!("registry load should fail"),
        Err(error) => error,
    }
}

fn skill_md(name: &str) -> String {
    format!("---\nname: {name}\ndescription: Use for examples.\n---\nbody")
}
