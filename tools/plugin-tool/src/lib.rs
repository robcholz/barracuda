//! Plugin discovery and System registry synchronization.

use std::{error::Error, fs, path::Path};

const MANIFEST_BEGIN: &str = "# BEGIN GENERATED PLUGINS";
const MANIFEST_END: &str = "# END GENERATED PLUGINS";
const SOURCE_BEGIN: &str = "// BEGIN GENERATED PLUGINS";
const SOURCE_END: &str = "// END GENERATED PLUGINS";

#[derive(Debug)]
struct Plugin {
    directory: String,
    package: String,
    crate_name: String,
    entry: String,
}

/// Synchronizes or validates the System registry against `plugins/`.
///
/// When `check` is true no files are written and a stale registry is reported
/// as an error.
///
/// # Errors
///
/// Returns an error when Plugin discovery, metadata parsing, generated block
/// validation, or filesystem access fails.
pub fn sync(root: &Path, check: bool) -> Result<(), Box<dyn Error>> {
    let plugins = discover(root)?;
    let system_manifest = root.join("core/system/Cargo.toml");
    let system_source = root.join("core/system/src/lib.rs");
    let manifest = replace_block(
        &fs::read_to_string(&system_manifest)?,
        MANIFEST_BEGIN,
        MANIFEST_END,
        &render_dependencies(&plugins),
    )?;
    let source = replace_block(
        &fs::read_to_string(&system_source)?,
        SOURCE_BEGIN,
        SOURCE_END,
        &render_registrations(&plugins),
    )?;
    let stale = manifest != fs::read_to_string(&system_manifest)?
        || source != fs::read_to_string(&system_source)?;
    if check && stale {
        return Err("Plugin registry is stale; run `cargo plugin sync`".into());
    }
    if !check {
        fs::write(system_manifest, manifest)?;
        fs::write(system_source, source)?;
    }
    Ok(())
}

fn discover(root: &Path) -> Result<Vec<Plugin>, Box<dyn Error>> {
    let mut plugins = Vec::new();
    for entry in fs::read_dir(root.join("plugins"))? {
        let directory = entry?;
        if !directory.file_type()?.is_dir() {
            continue;
        }
        let directory_name = directory.file_name().into_string().map_err(|_| {
            format!(
                "Plugin directory is not valid UTF-8: {:?}",
                directory.path()
            )
        })?;
        let plugin_root = directory.path();
        let manifest_path = plugin_root.join("crates/plugin/Cargo.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let manifest = fs::read_to_string(&manifest_path)?;
        let package = package_name(&manifest)
            .ok_or_else(|| format!("missing package name in {}", manifest_path.display()))?;
        let source_path = plugin_root.join("crates/plugin/src/lib.rs");
        let entry = plugin_entry(&fs::read_to_string(&source_path)?).ok_or_else(|| {
            format!(
                "expected one Plugin implementation in {}",
                source_path.display()
            )
        })?;
        plugins.push(Plugin {
            directory: directory_name,
            crate_name: package.replace('-', "_"),
            package,
            entry,
        });
    }
    plugins.sort_by(|left, right| left.directory.cmp(&right.directory));
    Ok(plugins)
}

fn package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package {
            let value = line.strip_prefix("name")?.trim_start();
            let value = value.strip_prefix('=')?.trim();
            return value.strip_prefix('"')?.strip_suffix('"').map(String::from);
        }
    }
    None
}

fn plugin_entry(source: &str) -> Option<String> {
    const PREFIX: &str = "impl<const M: usize> Plugin<M> for ";
    let entries = source
        .lines()
        .filter_map(|line| line.trim().strip_prefix(PREFIX))
        .filter_map(|remainder| remainder.split_whitespace().next())
        .filter(|entry| *entry != "Dependency")
        .map(String::from)
        .collect::<Vec<_>>();
    match entries.as_slice() {
        [entry] => Some(entry.clone()),
        _ => None,
    }
}

fn render_dependencies(plugins: &[Plugin]) -> String {
    plugins
        .iter()
        .map(|plugin| {
            format!(
                "{} = {{ path = \"../../plugins/{}/crates/plugin\" }}",
                plugin.package, plugin.directory
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_registrations(plugins: &[Plugin]) -> String {
    let entries = plugins
        .iter()
        .map(|plugin| {
            format!(
                "            {}::{}::new(&plugin_context),",
                plugin.crate_name, plugin.entry
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("        register_plugins!(plugins, router;\n{entries}\n        );")
}

fn replace_block(text: &str, begin: &str, end: &str, body: &str) -> Result<String, Box<dyn Error>> {
    let begin_offset = text
        .find(begin)
        .ok_or_else(|| format!("missing generated block marker `{begin}`"))?;
    let line_start = text[..begin_offset]
        .rfind('\n')
        .map_or(0, |offset| offset + 1);
    let indentation = &text[line_start..begin_offset];
    if !indentation.bytes().all(|byte| matches!(byte, b' ' | b'\t')) {
        return Err(format!("invalid indentation before `{begin}`").into());
    }
    let end_offset = text[begin_offset..]
        .find(end)
        .map(|offset| begin_offset + offset)
        .ok_or_else(|| format!("missing generated block marker `{end}`"))?;
    let line_end = text[end_offset..]
        .find('\n')
        .map_or(text.len(), |offset| end_offset + offset);
    let mut updated = String::with_capacity(text.len() + body.len());
    updated.push_str(&text[..line_start]);
    updated.push_str(indentation);
    updated.push_str(begin);
    updated.push('\n');
    updated.push_str(body);
    updated.push('\n');
    updated.push_str(indentation);
    updated.push_str(end);
    updated.push_str(&text[line_end..]);
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::{package_name, plugin_entry, replace_block};

    #[test]
    fn reads_plugin_metadata() {
        assert_eq!(
            package_name("[package]\nname = \"barracuda-demo-plugin\"\nversion = \"0.1.0\"\n"),
            Some(String::from("barracuda-demo-plugin"))
        );
        assert_eq!(
            plugin_entry("impl<const M: usize> Plugin<M> for DemoPlugin {\n}"),
            Some(String::from("DemoPlugin"))
        );
    }

    #[test]
    fn replaces_an_indented_generated_block() {
        let source = "fn register() {\n    // BEGIN\n    old();\n    // END\n}\n";
        assert_eq!(
            replace_block(source, "// BEGIN", "// END", "    new();")
                .as_deref()
                .ok(),
            Some("fn register() {\n    // BEGIN\n    new();\n    // END\n}\n")
        );
    }
}
