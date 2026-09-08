# Agent Skills

The Agent Skills subsystem discovers instruction packages, projects their
metadata into model context, and reads package content only when an Agent asks
for it. It owns skill identity and resource containment while filesystem
mounting, persistence, and image composition remain outside the subsystem.

## Boundary

~~~text
System-owned VFS
  +-- /data/skills ---------------------+
  `-- /workspace/resources/skills ------+--> Skill Registry
                                                |
                                                +--> catalog metadata
                                                +--> SKILL.md reader
                                                `--> bounded resource reader
                                                          |
                                                          v
                                                   Agent skill Tools
~~~

The writable data root contains user-installed packages. The shared resources
root contains immutable packages contributed by the selected Plugin set. Both
roots supply one catalog and retain their existing filesystem lifecycle.

## Catalog and identity

Each immediate child of a configured root is a skill package named by its
directory and described by its `SKILL.md` frontmatter. The registry publishes
immutable, versioned catalog snapshots sorted by skill name.

A skill name identifies exactly one complete package across every configured
root. Root order has no selection meaning. Discovering the same name in two
locations is a catalog error that identifies both directories. Initial
discovery fails on that error; reload preserves the previous valid snapshot.

The catalog retains discovery metadata and the resolved package directory.
Prompt context contains only the name and description. Filesystem directories
remain an internal registry detail.

## Progressive loading

An Agent first receives the compact catalog. `skill_read` loads the Markdown
instructions below one package's frontmatter. Instructions may explicitly name
additional files such as `references/guidelines.md` or
`references/examples.md`; `skill_resource_read` loads those files in bounded
UTF-8 pages.

The resource reader accepts a skill name and a relative path. It resolves the
package directory from the active catalog and accepts only regular files whose
path remains inside that directory. Absolute paths, parent traversal, and
filesystem paths supplied by the model are rejected. Reading script source does
not grant permission to execute it.

Callers select a page size from 4 bytes to 16 KiB. The lower bound can contain
any single UTF-8 scalar value, while the upper bound keeps each read allocation
bounded.

## Reload

Reload scans all configured roots and constructs a complete replacement
snapshot before publication. Successful reload atomically advances the catalog
version. Any malformed package, duplicate name, or filesystem failure leaves
the active snapshot unchanged so existing Agents continue to observe one
coherent catalog.

## Invariants

- Skill names are globally unique across configured roots.
- Filesystem location never selects between duplicate names.
- A resource is resolved from the directory recorded for its catalog entry.
- Model-facing resource paths are relative and contained within one package.
- Resource reads are bounded, paginated, read-only, and UTF-8.
- Catalog reload publishes a complete valid snapshot or preserves the previous one.
