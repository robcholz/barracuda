//! Scan several skills roots at once, and see how root priority resolves a
//! clashing id.
//!
//! Run with: `cargo run --example multi_root --target x86_64-unknown-linux-gnu`
//!
//! Mirrors the firmware layout: user-installed skills under the writable DATA
//! root can shadow firmware-baked skills under the read-only SYSTEM root.

#![allow(clippy::arc_with_non_send_sync)]

use std::sync::Arc;

use barracuda_agent_skill::{FsSkillRegistry, SkillName};
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
            .set_root("data")
            .await?
            .set_root("system")
            .await?,
    );
    let mut set = registry.skill_set();
    println!("== merged catalog from data + system ==");
    print!("{}", set.catalog_context());

    // Now use a collision: the same id `time` exists in both roots, and the
    // earlier DATA root shadows the later SYSTEM root.
    let filesystem = memory_vfs().await?;
    filesystem
        .write_atomic("system/time/SKILL.md", &skill_md("time", "baked"))
        .await?;
    filesystem
        .write_atomic("data/time/SKILL.md", &skill_md("time", "installed"))
        .await?;

    println!("\n== scanning roots with a clashing id ==");
    let registry = Arc::new(
        FsSkillRegistry::new(filesystem)
            .set_root("data")
            .await?
            .set_root("system")
            .await?,
    );
    let mut set = registry.skill_set();
    let catalog = set.catalog_context().to_string();
    println!("{catalog}");
    assert!(catalog.contains("- time: installed"));
    let document = set.read_skill(&SkillName::new("time")).await?;
    assert!(document.content().contains("# body"));

    Ok(())
}
