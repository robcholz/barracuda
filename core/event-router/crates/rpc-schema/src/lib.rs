//! Host-side JSON Schema bake pipeline for `#[rpc_dynamic]` RPC methods.
//!
//! This crate runs on the host — from a `build.rs` or a host tool — never on the
//! device. A request type opts in with [`register!`], which submits a
//! [`SchemaEntry`] to an [`inventory`] collection. [`bake_all`] walks that
//! collection and writes one `<name>.json` file per type, using `schemars` to
//! derive the schema from the Rust type (so it tracks `serde` attributes and
//! cannot drift from the wire contract).
//!
//! A consuming crate's `build.rs` imports this crate and calls [`bake_all`] into
//! `OUT_DIR`, then sets the `rpc_schema_baked` cfg; `#[rpc_dynamic]` embeds the
//! baked file with `include_str!`. `schemars` is a build-dependency only, so
//! nothing here reaches the firmware.

use std::fs;
use std::io;
use std::path::Path;

pub use inventory;
pub use schemars;
pub use serde_json;

/// One request type registered to be baked into a JSON Schema file.
pub struct SchemaEntry {
    /// File stem: the schema is written to `<name>.json`.
    pub name: &'static str,
    /// Produces the schema document for this type.
    pub json: fn() -> String,
}

inventory::collect!(SchemaEntry);

/// Writes every registered schema to `<out_dir>/<name>.json`.
///
/// # Errors
///
/// Returns any filesystem error encountered while writing a schema file.
pub fn bake_all(out_dir: &Path) -> io::Result<()> {
    for entry in inventory::iter::<SchemaEntry> {
        let path = out_dir.join(format!("{}.json", entry.name));
        fs::write(path, (entry.json)())?;
    }
    Ok(())
}

/// Registers a request type for baking.
///
/// The type must derive [`schemars::JsonSchema`]. Invoke it with a bare type
/// name so the baked file matches the `type Request` name `#[rpc_dynamic]` embeds:
///
/// ```ignore
/// barracuda_rpc_schema::register!(SetLevelRequest);
/// ```
///
/// The schema is baked with LLM-friendly settings: subschemas are inlined
/// (no `$ref`/`$defs`), `Option` fields use a nullable type array instead of
/// `anyOf`, and the meta-`$schema` key is omitted.
#[macro_export]
macro_rules! register {
    ($ty:ty) => {
        $crate::inventory::submit! {
            $crate::SchemaEntry {
                name: ::core::stringify!($ty),
                json: || {
                    let mut settings = $crate::schemars::gen::SchemaSettings::draft2019_09();
                    settings.inline_subschemas = true;
                    settings.option_nullable = true;
                    settings.meta_schema = None;
                    let mut generator = $crate::schemars::gen::SchemaGenerator::new(settings);
                    let schema = generator.root_schema_for::<$ty>();
                    $crate::serde_json::to_string_pretty(&schema).expect("serialize JSON schema")
                },
            }
        }
    };
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[derive(schemars::JsonSchema)]
    struct SetLevelRequest {
        #[allow(dead_code)]
        session: u32,
        #[allow(dead_code)]
        level: u32,
    }

    register!(SetLevelRequest);

    #[test]
    fn bake_all_writes_a_schema_file_per_registered_type() {
        let dir = std::env::temp_dir().join(format!("rpc-schema-bake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        bake_all(&dir).unwrap();

        let baked = std::fs::read_to_string(dir.join("SetLevelRequest.json")).unwrap();
        assert!(baked.contains("\"session\""), "{baked}");
        assert!(baked.contains("\"level\""), "{baked}");
        assert!(baked.contains("\"type\": \"object\""), "{baked}");
        assert!(!baked.contains("$defs"), "{baked}");
        assert!(!baked.contains("$ref"), "{baked}");
        assert!(!baked.contains("$schema"), "{baked}");

        std::fs::remove_dir_all(&dir).ok();
    }
}
