//! Read skills through a [`SkillSet`] and return one-shot document content.
//!
//! Run with: `cargo run --example load_context --target x86_64-unknown-linux-gnu`

use std::sync::Arc;

use claw_interface::{ClawFs, MemFs};
use claw_skill::{FsSkillRegistry, SkillName};

fn skill_md(name: &str, description: &str, body: &str) -> Vec<u8> {
    format!("---\nname: {name}\ndescription: {description}\n---\n{body}").into_bytes()
}

fn main() -> anyhow::Result<()> {
    let filesystem = Arc::new(MemFs::new());
    filesystem.write_atomic(
        "skills/board-hardware-info/SKILL.md",
        &skill_md(
            "board-hardware-info",
            "Board GPIO and peripheral reference.",
            "# Board hardware\nGPIO map ...",
        ),
    )?;
    filesystem.write_atomic(
        "skills/light-switch/SKILL.md",
        &skill_md(
            "light-switch",
            "Control board lights.",
            "# Light switch\nCall the light capability ...",
        ),
    )?;

    let registry = Arc::new(FsSkillRegistry::new(filesystem).set_root("skills")?);
    let mut set = registry.skill_set();

    println!("== catalog context ==\n{}", set.catalog_context());

    let document = set.read_skill(&SkillName::new("light-switch"))?;
    println!("== skill instructions ==\n{}", document.content());

    Ok(())
}
