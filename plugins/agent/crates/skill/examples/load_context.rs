//! Read skills through a [`SkillSet`] and return one-shot document content.
//!
//! Run with: `cargo run --example load_context --target x86_64-unknown-linux-gnu`

#![allow(clippy::arc_with_non_send_sync)]

use std::sync::Arc;

use barracuda_agent_skill::{FsSkillRegistry, SkillName};
use barracuda_platform_test::memory_vfs;
use futures_lite::future::block_on;

fn skill_md(name: &str, description: &str, body: &str) -> Vec<u8> {
    format!("---\nname: {name}\ndescription: {description}\n---\n{body}").into_bytes()
}

fn main() -> anyhow::Result<()> {
    block_on(run())
}

async fn run() -> anyhow::Result<()> {
    let filesystem = memory_vfs().await?;
    filesystem
        .write_atomic(
            "skills/board-hardware-info/SKILL.md",
            &skill_md(
                "board-hardware-info",
                "Board GPIO and peripheral reference.",
                "# Board hardware\nGPIO map ...",
            ),
        )
        .await?;
    filesystem
        .write_atomic(
            "skills/light-switch/SKILL.md",
            &skill_md(
                "light-switch",
                "Control board lights.",
                "# Light switch\nCall the light capability ...",
            ),
        )
        .await?;

    filesystem
        .write_atomic(
            "skills/light-switch/references/examples.md",
            b"# Examples\n\nTurn the status light on.",
        )
        .await?;

    let registry = Arc::new(FsSkillRegistry::new(filesystem).add_root("skills").await?);
    let mut set = registry.skill_set();

    println!("== catalog context ==\n{}", set.catalog_context());

    let document = set.read_skill(&SkillName::new("light-switch")).await?;
    println!("== skill instructions ==\n{}", document.content());

    let resource = set
        .read_resource(
            &SkillName::new("light-switch"),
            "references/examples.md",
            0,
            4096,
        )
        .await?;
    println!("== referenced examples ==\n{}", resource.content());

    Ok(())
}
