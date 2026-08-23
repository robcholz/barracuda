#![no_std]

//! Filesystem-backed runtime for standard [Agent Skills](https://agentskills.io).
//!
//! [`FsSkillRegistry`] scans priority-ordered roots such as DATA then SYSTEM.
//! [`SkillSet`] is the per-agent projection that renders the catalog and
//! reads one `SKILL.md` document on demand. Reading a skill returns an owned
//! [`SkillDocument`]; it does not create persistent activation state.

extern crate alloc;

mod document;
mod registry;
mod skill_set;

pub use document::{Skill, SkillDocument, SkillError, SkillName};
pub use registry::{
    CatalogSnapshot, EmptySkillRegistry, FsSkillRegistry, SkillRegistry, SkillRegistryVersion,
};
pub use skill_set::SkillSet;
