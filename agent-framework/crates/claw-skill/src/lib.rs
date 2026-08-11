#![no_std]

//! Filesystem-backed runtime for standard [Agent Skills](https://agentskills.io).
//!
//! [`FsSkillRegistry`] scans priority-ordered roots such as DATA then SYSTEM.
//! [`SkillSet`] is the per-agent projection that renders the catalog and
//! reads one `SKILL.md` document on demand. Reading a skill returns an owned
//! [`SkillDocument`]; it does not create persistent activation state.

extern crate alloc;

mod registry;
mod skill;
mod skill_set;

pub use registry::{
    CatalogSnapshot, EmptySkillRegistry, FsSkillRegistry, SkillRegistry, SkillRegistryVersion,
};
pub use skill::{Skill, SkillDocument, SkillError, SkillName};
pub use skill_set::SkillSet;
