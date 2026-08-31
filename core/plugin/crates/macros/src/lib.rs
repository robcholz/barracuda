//! Compile-time access to a Plugin's `plugin.toml` identity.

use std::{env, fs, path::PathBuf};

use barracuda_plugin_manifest::{parse, PluginManifest};
use syn::{parse_macro_input, ItemStruct};

/// Bakes a Plugin's identity and dependencies from its `plugin.toml`.
///
/// The calling crate must use the standard `plugins/<name>/crates/plugin`
/// layout. Apply this attribute to its [`Plugin`](barracuda_plugin_manager::Plugin)
/// type; the attribute implements
/// [`PluginDeclaration`](barracuda_plugin_manager::PluginDeclaration).
/// Invalid or missing manifests produce a compile error.
#[proc_macro_attribute]
pub fn plugin(
    args: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    if !args.is_empty() {
        return compile_error("plugin does not accept arguments");
    }

    let item = parse_macro_input!(input as ItemStruct);

    match read_plugin_manifest() {
        Ok((manifest, path)) => {
            let id = manifest.id();
            let dependencies = manifest.dependencies();
            let path = path.to_string_lossy().into_owned();
            let name = &item.ident;
            let (implementation_generics, type_generics, where_clause) =
                item.generics.split_for_impl();
            quote::quote! {
                #item

                impl #implementation_generics ::barracuda_plugin_manager::PluginDeclaration
                    for #name #type_generics #where_clause
                {
                    const ID: &'static str = {
                        const _: &str = include_str!(#path);
                        #id
                    };
                    const DEPENDS_ON: &'static [&'static str] = &[#(#dependencies),*];
                }
            }
            .into()
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
    let manifest = parse(&contents)
        .map_err(|error| format!("invalid Plugin manifest {}: {error}", path.display()))?;
    Ok((manifest, path))
}

fn compile_error(message: &str) -> proc_macro::TokenStream {
    format!("compile_error!({message:?})")
        .parse()
        .unwrap_or_else(|_| proc_macro::TokenStream::new())
}
