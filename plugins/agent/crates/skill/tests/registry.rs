#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use barracuda_agent_skill::{
    CatalogSnapshot, FsSkillRegistry, Skill, SkillError, SkillName, SkillRegistry,
};
use barracuda_fs::FileSystem;
use barracuda_platform_test::MemFs;

struct ExternalRegistry {
    catalog: Arc<CatalogSnapshot>,
}

impl SkillRegistry for ExternalRegistry {
    fn catalog(&self) -> Arc<CatalogSnapshot> {
        Arc::clone(&self.catalog)
    }

    fn reload(&self) -> Result<(), SkillError> {
        Ok(())
    }

    fn read_document_into(&self, name: &SkillName, out: &mut String) -> Result<(), SkillError> {
        if self.catalog.get(name).is_none() {
            return Err(SkillError::NotFound(name.clone()));
        }
        out.push_str("body");
        Ok(())
    }
}

#[test]
fn public_registry_trait_drives_skill_set() {
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
            .unwrap()
            .content(),
        "body"
    );
}

#[test]
fn registry_parses_standard_frontmatter() {
    let filesystem = MemFs::new();
    write_skill(
        &filesystem,
        "example-skill",
        "---\nname: example-skill\ndescription: >\n  Does a useful thing.\n  Use for examples.\nlicense: Apache-2.0\ncompatibility: Requires network access\nmetadata:\n  author: example-org\n  version: \"1.0\"\nallowed-tools: Read Bash(git:*)\n---\n# Instructions\n\nRead references/GUIDE.md.\n",
    );

    let registry = Arc::new(FsSkillRegistry::new(filesystem).set_root("skills").unwrap());
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
    let document = skills.read_skill(&SkillName::new("example-skill")).unwrap();
    assert_eq!(
        document.content(),
        "# Instructions\n\nRead references/GUIDE.md."
    );
    assert_eq!(document.directory(), Some("skills/example-skill"));
}

#[test]
fn json_frontmatter_is_not_accepted_as_legacy_format() {
    let filesystem = MemFs::new();
    write_skill(
        &filesystem,
        "example-skill",
        "---\n{\"name\":\"example-skill\",\"description\":\"legacy\"}\n---\nbody",
    );

    assert!(matches!(
        registry_error(filesystem),
        SkillError::InvalidYaml(_, _)
    ));
}

#[test]
fn legacy_metadata_shapes_are_rejected() {
    let filesystem = MemFs::new();
    write_skill(
        &filesystem,
        "example-skill",
        "---\nname: example-skill\ndescription: Use for examples.\nmetadata:\n  cap_groups:\n    - cap_lua\n  manage_mode: readonly\n---\nbody",
    );

    assert!(matches!(
        registry_error(filesystem),
        SkillError::InvalidYaml(_, _)
    ));
}

#[test]
fn missing_opening_fence_errors() {
    let filesystem = MemFs::new();
    write_skill(&filesystem, "example-skill", "no frontmatter");
    assert!(matches!(
        registry_error(filesystem),
        SkillError::MissingOpeningFence(_)
    ));
}

#[test]
fn missing_closing_fence_errors() {
    let filesystem = MemFs::new();
    write_skill(
        &filesystem,
        "example-skill",
        "---\nname: example-skill\ndescription: Use for examples.\n",
    );
    assert!(matches!(
        registry_error(filesystem),
        SkillError::MissingClosingFence(_)
    ));
}

#[test]
fn name_must_match_directory() {
    let filesystem = MemFs::new();
    write_skill(&filesystem, "example-skill", &skill_md("other-skill"));
    assert!(matches!(
        registry_error(filesystem),
        SkillError::InvalidFrontmatter(_, _)
    ));
}

#[test]
fn name_rejects_legacy_underscores() {
    let filesystem = MemFs::new();
    write_skill(&filesystem, "legacy_skill", &skill_md("legacy_skill"));
    assert!(matches!(
        registry_error(filesystem),
        SkillError::InvalidFrontmatter(_, _)
    ));
}

#[test]
fn closing_fence_must_occupy_its_own_line() {
    let filesystem = MemFs::new();
    write_skill(
        &filesystem,
        "example-skill",
        "---\nname: example-skill\ndescription: Use for examples.\n---not-a-fence\nbody",
    );
    assert!(matches!(
        registry_error(filesystem),
        SkillError::MissingClosingFence(_)
    ));
}

fn write_skill(filesystem: &MemFs, name: &str, document: &str) {
    filesystem
        .write_atomic(&format!("skills/{name}/SKILL.md"), document.as_bytes())
        .unwrap();
}

fn registry_error(filesystem: MemFs) -> SkillError {
    match FsSkillRegistry::new(filesystem).set_root("skills") {
        Ok(_) => panic!("registry load should fail"),
        Err(error) => error,
    }
}

fn skill_md(name: &str) -> String {
    format!("---\nname: {name}\ndescription: Use for examples.\n---\nbody")
}
