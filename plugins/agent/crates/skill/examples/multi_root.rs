//! Scan several skills roots into one globally unique catalog.
//!
//! Run with: `cargo run --example multi_root --target x86_64-unknown-linux-gnu`
//!
//! Mirrors the firmware layout: user-installed and firmware-baked skills have
//! different lifecycles but share one namespace.

#![allow(clippy::arc_with_non_send_sync)]

use std::sync::Arc;

use barracuda_agent_skill::FsSkillRegistry;
use barracuda_platform_test::memory_vfs;
use futures_lite::future::block_on;

fn skill_md(name: &str, description: &str) -> Vec<u8> {
    format!("---\nname: {name}\ndescription: {description}\n---\n# body\n").into_bytes()
}

fn main() -> anyhow::Result<()> {
    block_on(run())
}

async fn run() -> anyhow::Result<()> {
    // Two distinct roots, each contributing different skills.
    let filesystem = memory_vfs().await?;
    filesystem
        .write_atomic(
            "system/time/SKILL.md",
            &skill_md("time", "Built-in time helper."),
        )
        .await?;
    filesystem
        .write_atomic(
            "data/notes/SKILL.md",
            &skill_md("notes", "User-installed notes skill."),
        )
        .await?;

    let registry = Arc::new(
        FsSkillRegistry::new(filesystem.clone())
            .add_root("data")
            .await?
            .add_root("system")
            .await?,
    );
    let mut set = registry.skill_set();
    println!("== merged catalog from data + system ==");
    print!("{}", set.catalog_context());

    // A collision is rejected regardless of root order.
    let filesystem = memory_vfs().await?;
    filesystem
        .write_atomic("system/time/SKILL.md", &skill_md("time", "baked"))
        .await?;
    filesystem
        .write_atomic("data/time/SKILL.md", &skill_md("time", "installed"))
        .await?;

    println!("\n== scanning roots with a clashing id ==");
    let duplicate = FsSkillRegistry::new(filesystem)
        .add_root("data")
        .await?
        .add_root("system")
        .await;
    assert!(duplicate.is_err());
    if let Err(error) = duplicate {
        println!("{error}");
    }

    Ok(())
}
