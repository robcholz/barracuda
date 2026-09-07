#![no_std]

//! Filesystem-backed runtime for standard [Agent Skills](https://agentskills.io).
//!
//! [`FsSkillRegistry`] scans multiple roots into one globally unique catalog.
//! [`SkillSet`] is the per-agent projection that renders the catalog and
//! reads one `SKILL.md` document on demand. Reading a skill returns an owned
//! [`SkillDocument`]; it does not create persistent activation state.

extern crate alloc;

mod document;
mod registry;
mod skill_set;

pub use document::{
    Skill, SkillDocument, SkillError, SkillName, SkillResourcePage, DEFAULT_RESOURCE_READ_BYTES,
    MAX_RESOURCE_READ_BYTES,
};
pub use registry::{
    CatalogSnapshot, EmptySkillRegistry, FsSkillRegistry, SkillRegistry, SkillRegistryVersion,
};
pub use skill_set::SkillSet;
