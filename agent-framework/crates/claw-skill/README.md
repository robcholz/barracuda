# claw-skill

`no_std` filesystem runtime for standard [Agent Skills](https://agentskills.io/specification).

A skill is a directory containing `SKILL.md` and optional `scripts/`,
`references/`, and `assets/` resources. `FsSkillRegistry` scans one or more
priority-ordered roots. It retains only discovery metadata and reads the
Markdown instructions on demand.

## Standard format

```yaml
---
name: light-switch
description: Turn a board light on or off. Use when the user asks to control lighting.
license: Apache-2.0
compatibility: Requires an ESP-Claw board with a configured light
metadata:
  author: example-org
  version: "1.0"
allowed-tools: lua_run_script
---

# Light switch

Run `scripts/switch.lua`.
```

`name` and `description` are required. `license`, `compatibility`, `metadata`,
and the experimental `allowed-tools` field are optional. Frontmatter is YAML,
`metadata` values are strings, names use lowercase letters, digits, and
hyphens, and the name must match the parent directory.

This crate deliberately does not accept ESP-Claw's former JSON frontmatter or
its `cap_groups`, `manage_mode`, category, peripheral, and tag schema.

## API

- `FsSkillRegistry` discovers skills from filesystem roots.
- `CatalogSnapshot` is an immutable, versioned discovery snapshot.
- `Skill` contains the standard frontmatter fields.
- `SkillSet::catalog_context()` renders name and description for prompt discovery.
- `SkillSet::list_skills()` returns the same discovery fields as JSON.
- `SkillSet::read_skill()` returns the Markdown instructions below frontmatter
  and the skill directory used to resolve relative resources.
- `SkillSet::reload()` rescans the roots without replacing a valid snapshot on failure.

```rust
use std::sync::Arc;

use claw_fs::MemFs;
use claw_skill::{FsSkillRegistry, SkillName};

fn build(filesystem: Arc<MemFs>) -> Result<(), claw_skill::SkillError> {
    let registry = Arc::new(
        FsSkillRegistry::new(filesystem)
            .set_root("data/skills")?
            .set_root("system/skills")?,
    );
    let mut skills = registry.skill_set();

    println!("{}", skills.list_skills());
    let document = skills.read_skill(&SkillName::new("light-switch"))?;
    println!("{}", document.content());
    Ok(())
}
```

Roots are priority ordered, so a skill in an earlier DATA root shadows the same
name in a later SYSTEM root. The crate does not install, register, enable,
disable, or persist activation state. Resource paths in skill instructions stay
relative to the skill root as required by the standard.
