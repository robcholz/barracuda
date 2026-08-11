//! Scan a skills directory and render the available-skills catalog.
//!
//! Run with: `cargo run --example catalog --target x86_64-unknown-linux-gnu`
//!
//! Uses an in-memory [`MemFs`] so the example is self-contained — in the
//! firmware the same [`FsSkillRegistry`] is configured over on-device `ClawFs`.

use std::sync::Arc;

use claw_interface::{ClawFs, MemFs};
use claw_skill::FsSkillRegistry;

/// Build a standard `SKILL.md` with YAML frontmatter and a Markdown body.
fn skill_md(name: &str, description: &str, body: &str) -> Vec<u8> {
    format!("---\nname: {name}\ndescription: {description}\n---\n{body}").into_bytes()
}

fn main() -> anyhow::Result<()> {
    // Lay out two skills under the `skills` root.
    let filesystem = Arc::new(MemFs::new());
    filesystem.write_atomic(
        "skills/weather-search/SKILL.md",
        &skill_md(
            "weather-search",
            "Answer weather and forecast questions via web search.",
            "# Weather\n...",
        ),
    )?;
    filesystem.write_atomic(
        "skills/light-switch/SKILL.md",
        &skill_md(
            "light-switch",
            "Turn board lights and LED strips on or off.",
            "# Light switch\n...",
        ),
    )?;

    let registry = Arc::new(FsSkillRegistry::new(filesystem).set_root("skills")?);
    let mut set = registry.skill_set();

    println!("== JSON catalog ==");
    println!("{}", set.list_skills());

    println!("\n== prompt catalog ==");
    print!("{}", set.catalog_context());

    Ok(())
}
