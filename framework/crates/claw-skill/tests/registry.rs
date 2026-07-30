#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use claw_interface::{ClawFs, MemFs};
use claw_skill::{
    CatalogSnapshot, FsSkillRegistry, Skill, SkillError, SkillFrontmatterMetadata, SkillId,
    SkillManageMode, SkillRegistry,
};

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

    fn load_document_into(&self, id: &SkillId, out: &mut String) -> Result<(), SkillError> {
        if self.catalog.get(id).is_none() {
            return Err(SkillError::NotFound(id.clone()));
        }
        out.push_str("<skill_content name=\"external\">\nbody\n</skill_content>");
        Ok(())
    }
}

#[test]
fn public_registry_trait_drives_skill_set() {
    let skill = Skill::from_catalog_entry(
        SkillId::new("external"),
        "external backend".to_owned(),
        "external/SKILL.md".to_owned(),
        SkillFrontmatterMetadata::new(
            Vec::new(),
            SkillManageMode::Readonly,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ),
    );
    let registry: Arc<dyn SkillRegistry> = Arc::new(ExternalRegistry {
        catalog: Arc::new(CatalogSnapshot::from_skills(1, vec![skill])),
    });
    let mut skills = registry.skill_set();

    assert!(skills.catalog_context().contains("external backend"));
    assert_eq!(
        skills
            .activate_skill(&SkillId::new("external"))
            .unwrap()
            .content(),
        "<skill_content name=\"external\">\nbody\n</skill_content>"
    );
}

#[test]
fn registry_parses_master_front_matter_shape() {
    let filesystem = Arc::new(MemFs::new());
    write_skill(filesystem.as_ref(), "x", &skill_md("x"));

    let registry = Arc::new(FsSkillRegistry::new(filesystem).set_root("skills").unwrap());
    let mut skills = registry.skill_set();
    let catalog: serde_json::Value = serde_json::from_str(skills.list_skill().unwrap()).unwrap();
    let rows = catalog.as_array().unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "x");
    assert_eq!(rows[0]["name"], "x");
    assert_eq!(rows[0]["description"], "d");
    assert_eq!(rows[0]["metadata"]["manage_mode"], "readonly");
    assert_eq!(rows[0]["file"], "x/SKILL.md");

    let document = skills.activate_skill(&SkillId::new("x")).unwrap();
    assert_eq!(
        document.content(),
        "<skill_content name=\"x\">\nbody\n</skill_content>"
    );
}

#[test]
fn missing_opening_fence_errors() {
    let filesystem = MemFs::new();
    write_skill(&filesystem, "x", "no front matter");

    let error = registry_error(filesystem);
    assert!(matches!(error, SkillError::MissingOpeningFence(_)));
}

#[test]
fn missing_close_fence_errors() {
    let filesystem = MemFs::new();
    write_skill(&filesystem, "x", "---\n{}\n");

    let error = registry_error(filesystem);
    assert!(matches!(error, SkillError::MissingClosingFence(_)));
}

#[test]
fn invalid_json_errors() {
    let filesystem = MemFs::new();
    write_skill(&filesystem, "x", "---\nnot json\n---\nbody");

    let error = registry_error(filesystem);
    assert!(matches!(error, SkillError::InvalidJson(_, _)));
}

#[test]
fn front_matter_name_must_match_directory() {
    let filesystem = MemFs::new();
    write_skill(&filesystem, "x", &skill_md("other"));

    let error = registry_error(filesystem);
    assert!(matches!(error, SkillError::InvalidFrontmatter(_, _)));
}

#[test]
fn skill_manage_mode_uses_canonical_labels_and_aliases() {
    let label: &'static str = SkillManageMode::Readonly.into();
    assert_eq!(label, "readonly");
    assert_eq!(
        SkillManageMode::try_from("readonly"),
        Ok(SkillManageMode::Readonly)
    );
    assert_eq!(
        SkillManageMode::try_from("web"),
        Ok(SkillManageMode::Readonly)
    );
    assert_eq!(
        SkillManageMode::try_from("runtime"),
        Ok(SkillManageMode::Runtime)
    );
}

fn write_skill(filesystem: &MemFs, id: &str, document: &str) {
    filesystem
        .write_atomic(&format!("skills/{id}/SKILL.md"), document.as_bytes())
        .unwrap();
}

fn registry_error(filesystem: MemFs) -> SkillError {
    match FsSkillRegistry::new(Arc::new(filesystem)).set_root("skills") {
        Ok(_) => panic!("registry load should fail"),
        Err(error) => error,
    }
}

fn skill_md(name: &str) -> String {
    format!(
        "---\n{{\"name\":\"{name}\",\"description\":\"d\",\"metadata\":{{\"manage_mode\":\"web\"}}}}\n---\nbody"
    )
}
