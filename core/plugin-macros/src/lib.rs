//! Compile-time access to a Plugin's `plugin.toml` identity.

use std::{env, fs, path::PathBuf};

use serde::Deserialize;
use syn::{parse_macro_input, ItemImpl};

#[derive(Deserialize)]
struct PluginManifest {
    id: String,
    #[serde(rename = "depends-on")]
    depends_on: Vec<String>,
}

/// Bakes a Plugin's identity and dependencies from its `plugin.toml`.
///
/// The calling crate must use the standard `plugins/<name>/crates/plugin`
/// layout. Apply this attribute to its [`Plugin`](barracuda_plugin_manager::Plugin)
/// implementation; the attribute supplies `Plugin::DEPENDS_ON` and `Plugin::id`.
/// Invalid or missing manifests produce a compile error.
#[proc_macro_attribute]
pub fn plugin(
    args: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    if !args.is_empty() {
        return compile_error("plugin does not accept arguments");
    }

    let mut implementation = parse_macro_input!(input as ItemImpl);

    match read_plugin_manifest() {
        Ok((manifest, path)) => {
            let id = manifest.id;
            let dependencies = manifest.depends_on;
            let path = path.to_string_lossy().into_owned();
            implementation.items.push(syn::parse_quote! {
                const DEPENDS_ON: &'static [&'static str] = &[#(#dependencies),*];
            });
            implementation.items.push(syn::parse_quote! {
                fn id(&self) -> &'static str {
                    const _: &str = include_str!(#path);
                    #id
                }
            });
            quote::quote!(#implementation).into()
        }
        Err(error) => compile_error(&error),
    }
}

fn read_plugin_manifest() -> Result<(PluginManifest, PathBuf), String> {
    let crate_dir = env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| String::from("CARGO_MANIFEST_DIR is not set"))?;
    let path = crate_dir.join("../../plugin.toml");
    let contents = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let manifest = toml::from_str::<PluginManifest>(&contents)
        .map_err(|error| format!("invalid Plugin manifest {}: {error}", path.display()))?;
    if manifest.id.trim().is_empty() {
        return Err(format!("Plugin ID in {} must not be empty", path.display()));
    }
    if manifest
        .depends_on
        .iter()
        .any(|dependency| dependency.trim().is_empty())
    {
        return Err(format!(
            "Plugin dependencies in {} must not be empty",
            path.display()
        ));
    }
    Ok((manifest, path))
}

fn compile_error(message: &str) -> proc_macro::TokenStream {
    format!("compile_error!({message:?})")
        .parse()
        .unwrap_or_else(|_| proc_macro::TokenStream::new())
}
