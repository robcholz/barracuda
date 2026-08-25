//! Scan a skills directory and render the available-skills catalog.
//!
//! Run with: `cargo run --example catalog --target x86_64-unknown-linux-gnu`
//!
//! Uses an in-memory VFS so the example is self-contained. Firmware receives
//! the same [`FsSkillRegistry`] over its plugin-private namespace.

#![allow(clippy::arc_with_non_send_sync)]

use std::sync::Arc;

use barracuda_agent_skill::FsSkillRegistry;
use barracuda_platform_test::memory_vfs;
use futures_lite::future::block_on;

/// Build a standard `SKILL.md` with YAML frontmatter and a Markdown body.
fn skill_md(name: &str, description: &str, body: &str) -> Vec<u8> {
    format!("---\nname: {name}\ndescription: {description}\n---\n{body}").into_bytes()
}

fn main() -> anyhow::Result<()> {
    block_on(run())
}

async fn run() -> anyhow::Result<()> {
    // Lay out two skills under the `skills` root.
    let filesystem = memory_vfs().await?;
    filesystem
        .write_atomic(
            "skills/weather-search/SKILL.md",
            &skill_md(
                "weather-search",
                "Answer weather and forecast questions via web search.",
                "# Weather\n...",
            ),
        )
        .await?;
    filesystem
        .write_atomic(
            "skills/light-switch/SKILL.md",
            &skill_md(
                "light-switch",
                "Turn board lights and LED strips on or off.",
                "# Light switch\n...",
            ),
        )
        .await?;

    let registry = Arc::new(FsSkillRegistry::new(filesystem).set_root("skills").await?);
    let mut set = registry.skill_set();

    println!("== JSON catalog ==");
    println!("{}", set.list_skills());

    println!("\n== prompt catalog ==");
    print!("{}", set.catalog_context());

    Ok(())
}
