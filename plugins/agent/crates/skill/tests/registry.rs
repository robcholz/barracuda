#![allow(clippy::unwrap_used, clippy::arc_with_non_send_sync)]

use std::{future::Future, pin::Pin, sync::Arc};

use barracuda_agent_skill::{
    CatalogSnapshot, FsSkillRegistry, Skill, SkillError, SkillName, SkillRegistry,
    SkillResourcePage,
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

    fn read_resource<'a>(
        &'a self,
        name: &'a SkillName,
        path: &'a str,
        offset: u64,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<SkillResourcePage, SkillError>> + 'a>> {
        Box::pin(async move {
            if self.catalog.get(name).is_none() {
                return Err(SkillError::NotFound(name.clone()));
            }
            Ok(SkillResourcePage::new(
                path.to_owned(),
                "example"
                    .chars()
                    .skip(offset as usize)
                    .take(limit)
                    .collect(),
                offset,
                limit.min(7_usize.saturating_sub(offset as usize)),
                None,
            ))
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
        assert_eq!(
            skills
                .read_resource(&SkillName::new("external"), "references/example.md", 0, 16)
                .await
                .unwrap()
                .content(),
            "example"
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
                .add_root("skills")
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
            .add_root("skills")
            .await
            .unwrap();
        assert!(registry
            .catalog()
            .get(&SkillName::new("example-skill"))
            .is_some());
    });
}

#[test]
fn duplicate_skill_names_across_roots_are_rejected_regardless_of_root_order() {
    block_on(async {
        for roots in [["data", "system"], ["system", "data"]] {
            let filesystem = memory_vfs().await.unwrap();
            write_skill_at(
                &filesystem,
                roots[0],
                "example-skill",
                &skill_md("example-skill"),
            )
            .await;
            write_skill_at(
                &filesystem,
                roots[1],
                "example-skill",
                &skill_md("example-skill"),
            )
            .await;

            let error = match FsSkillRegistry::new(filesystem)
                .add_root(roots[0])
                .await
                .unwrap()
                .add_root(roots[1])
                .await
            {
                Ok(_) => panic!("duplicate skill should fail"),
                Err(error) => error,
            };
            assert!(matches!(
                error,
                SkillError::DuplicateSkill { ref name, .. }
                    if name.as_str() == "example-skill"
            ));
        }
    });
}

#[test]
fn resource_reads_are_bounded_to_the_registered_skill_directory() {
    block_on(async {
        let filesystem = memory_vfs().await.unwrap();
        write_skill(&filesystem, "example-skill", &skill_md("example-skill")).await;
        filesystem
            .write_atomic("skills/example-skill/references/guide.md", b"hello world")
            .await
            .unwrap();
        let registry = Arc::new(
            FsSkillRegistry::new(filesystem)
                .add_root("skills")
                .await
                .unwrap(),
        );
        let skills = registry.skill_set();

        let first = skills
            .read_resource(
                &SkillName::new("example-skill"),
                "references/guide.md",
                0,
                5,
            )
            .await
            .unwrap();
        assert_eq!(first.content(), "hello");
        assert_eq!(first.offset(), 0);
        assert_eq!(first.bytes(), 5);
        assert_eq!(first.next_offset(), Some(5));

        let second = skills
            .read_resource(
                &SkillName::new("example-skill"),
                "references/guide.md",
                5,
                16,
            )
            .await
            .unwrap();
        assert_eq!(second.content(), " world");
        assert_eq!(second.next_offset(), None);

        assert!(matches!(
            skills
                .read_resource(&SkillName::new("example-skill"), "references", 0, 16,)
                .await,
            Err(SkillError::ResourceNotFile { .. })
        ));
        assert!(matches!(
            skills
                .read_resource(
                    &SkillName::new("example-skill"),
                    "references/guide.md",
                    12,
                    16,
                )
                .await,
            Err(SkillError::ResourceRangeOutOfBounds { .. })
        ));
        assert!(matches!(
            skills
                .read_resource(
                    &SkillName::new("example-skill"),
                    "references/guide.md",
                    0,
                    0,
                )
                .await,
            Err(SkillError::InvalidResourceLimit {
                limit: 0,
                min: 4,
                max: 16_384,
            })
        ));
        assert!(matches!(
            skills
                .read_resource(
                    &SkillName::new("example-skill"),
                    "references/guide.md",
                    0,
                    3,
                )
                .await,
            Err(SkillError::InvalidResourceLimit {
                limit: 3,
                min: 4,
                max: 16_384,
            })
        ));
        assert!(matches!(
            skills
                .read_resource(
                    &SkillName::new("missing-skill"),
                    "references/guide.md",
                    0,
                    16,
                )
                .await,
            Err(SkillError::NotFound(_))
        ));

        for path in [
            "/skills/secret.md",
            "../secret.md",
            "references/../../secret.md",
        ] {
            assert!(matches!(
                skills
                    .read_resource(&SkillName::new("example-skill"), path, 0, 16)
                    .await,
                Err(SkillError::InvalidResourcePath { .. })
            ));
        }
    });
}

#[test]
fn resource_read_rejects_non_utf8_pages() {
    block_on(async {
        let filesystem = memory_vfs().await.unwrap();
        write_skill(&filesystem, "example-skill", &skill_md("example-skill")).await;
        filesystem
            .write_atomic("skills/example-skill/references/binary", &[0xff, 0, 0, 0])
            .await
            .unwrap();
        let registry = Arc::new(
            FsSkillRegistry::new(filesystem)
                .add_root("skills")
                .await
                .unwrap(),
        );
        let skills = registry.skill_set();

        assert!(matches!(
            skills
                .read_resource(&SkillName::new("example-skill"), "references/binary", 0, 4,)
                .await,
            Err(SkillError::InvalidResourceUtf8 { .. })
        ));
    });
}

#[test]
fn resource_pages_end_on_utf8_boundaries() {
    block_on(async {
        let filesystem = memory_vfs().await.unwrap();
        write_skill(&filesystem, "example-skill", &skill_md("example-skill")).await;
        filesystem
            .write_atomic(
                "skills/example-skill/references/chinese.md",
                "你好".as_bytes(),
            )
            .await
            .unwrap();
        let registry = Arc::new(
            FsSkillRegistry::new(filesystem)
                .add_root("skills")
                .await
                .unwrap(),
        );
        let skills = registry.skill_set();

        let first = skills
            .read_resource(
                &SkillName::new("example-skill"),
                "references/chinese.md",
                0,
                4,
            )
            .await
            .unwrap();
        assert_eq!(first.content(), "你");
        assert_eq!(first.bytes(), 3);
        assert_eq!(first.next_offset(), Some(3));

        let second = skills
            .read_resource(
                &SkillName::new("example-skill"),
                "references/chinese.md",
                first.next_offset().unwrap(),
                4,
            )
            .await
            .unwrap();
        assert_eq!(second.content(), "好");
        assert_eq!(second.next_offset(), None);
    });
}

#[test]
fn failed_reload_keeps_the_previous_unique_catalog() {
    block_on(async {
        let filesystem = memory_vfs().await.unwrap();
        write_skill_at(&filesystem, "data", "notes", &skill_md("notes")).await;
        write_skill_at(&filesystem, "system", "time", &skill_md("time")).await;
        let registry = Arc::new(
            FsSkillRegistry::new(filesystem.clone())
                .add_root("data")
                .await
                .unwrap()
                .add_root("system")
                .await
                .unwrap(),
        );
        let version = registry.catalog().version();

        write_skill_at(&filesystem, "system", "notes", &skill_md("notes")).await;
        assert!(matches!(
            registry.reload().await,
            Err(SkillError::DuplicateSkill { .. })
        ));
        assert_eq!(registry.catalog().version(), version);
        assert!(registry.catalog().get(&SkillName::new("notes")).is_some());
        assert!(registry.catalog().get(&SkillName::new("time")).is_some());
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
    write_skill_at(filesystem, "skills", name, document).await;
}

async fn write_skill_at(filesystem: &ScopedVfs, root: &str, name: &str, document: &str) {
    filesystem
        .write_atomic(&format!("{root}/{name}/SKILL.md"), document.as_bytes())
        .await
        .unwrap();
}

async fn registry_error(filesystem: ScopedVfs) -> SkillError {
    match FsSkillRegistry::new(filesystem).add_root("skills").await {
        Ok(_) => panic!("registry load should fail"),
        Err(error) => error,
    }
}

fn skill_md(name: &str) -> String {
    format!("---\nname: {name}\ndescription: Use for examples.\n---\nbody")
}
