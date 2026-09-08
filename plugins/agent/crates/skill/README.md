# barracuda-agent-skill

`no_std` filesystem runtime for standard [Agent Skills](https://agentskills.io/specification).

A skill is a directory containing `SKILL.md` and optional `scripts/`,
`references/`, and `assets/` resources. `FsSkillRegistry` scans one or more
roots into a globally unique catalog. It retains only discovery metadata and
reads instructions and referenced resources on demand.

## Standard format

```yaml
---
name: light-switch
description: Turn a board light on or off. Use when the user asks to control lighting.
license: Apache-2.0
compatibility: Requires an barracuda board with a configured light
metadata:
  author: example-org
  version: "1.0"
allowed-tools: lua_run_script
---

# Light switch

Run `scripts/switch.lua`.
```

Selected Plugins bundle immutable packages at
`filesystem/workspace/resources/skills/<name>/`, which Agent sees at
`/workspace/resources/skills/<name>/`. User-installed packages live in the
Agent Plugin's durable `/data/skills/<name>/` tree. These locations differ only
in lifecycle; both contribute to the same unique catalog.

`name` and `description` are required. `license`, `compatibility`, `metadata`,
and the experimental `allowed-tools` field are optional. Frontmatter is YAML,
`metadata` values are strings, names use lowercase letters, digits, and
hyphens, and the name must match the parent directory.

This crate deliberately does not accept barracuda's former JSON frontmatter or
its `cap_groups`, `manage_mode`, category, peripheral, and tag schema.

## API

- `FsSkillRegistry` discovers skills from filesystem roots.
- `CatalogSnapshot` is an immutable, versioned discovery snapshot.
- `Skill` contains the standard frontmatter fields.
- `SkillSet::catalog_context()` renders name and description for prompt discovery.
- `SkillSet::list_skills()` returns the same discovery fields as JSON.
- `SkillSet::read_skill()` returns the Markdown instructions below frontmatter
  while keeping its filesystem directory internal to resource resolution.
- `SkillSet::read_resource()` returns a 4-byte to 16-KiB UTF-8 page from a
  relative regular file inside the uniquely registered skill directory.
- `SkillSet::reload()` rescans the roots without replacing a valid snapshot on failure.

```rust
use std::sync::Arc;

use barracuda_agent_skill::{FsSkillRegistry, SkillName};
use barracuda_platform_test::memory_vfs;
use futures_lite::future::block_on;

fn build() -> Result<(), barracuda_agent_skill::SkillError> {
    block_on(async {
    let filesystem = memory_vfs().await.unwrap();
    let registry = Arc::new(FsSkillRegistry::new(filesystem)
        .add_root("data/skills").await?
        .add_root("system/skills").await?);
    let mut skills = registry.skill_set();

    println!("{}", skills.list_skills());
    let document = skills.read_skill(&SkillName::new("light-switch")).await?;
    println!("{}", document.content());
    let guide = skills.read_resource(
        &SkillName::new("light-switch"),
        "references/guide.md",
        0,
        4096,
    ).await?;
    println!("{}", guide.content());
    Ok(())
    })
}
```

Root order does not affect selection. A skill name must be globally unique
across every configured root; a duplicate fails construction or reload instead
of selecting one copy. A failed reload preserves the previous valid snapshot.
The crate does not install, register, enable, disable, or persist activation
state. Resource paths stay relative to the selected skill directory and cannot
escape it.
