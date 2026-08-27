//! Plugin discovery, selection, and System registry synchronization.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use dialoguer::{theme::ColorfulTheme, MultiSelect};

const MANIFEST_BEGIN: &str = "# BEGIN GENERATED PLUGINS";
const MANIFEST_END: &str = "# END GENERATED PLUGINS";
const FEATURES_BEGIN: &str = "# BEGIN GENERATED PLUGIN FEATURES";
const FEATURES_END: &str = "# END GENERATED PLUGIN FEATURES";
const SOURCE_BEGIN: &str = "// BEGIN GENERATED PLUGINS";
const SOURCE_END: &str = "// END GENERATED PLUGINS";
const DISABLED_PATH: &str = ".barracuda/disabled-plugins";

#[derive(Debug)]
struct Plugin {
    directory: String,
    package: String,
    crate_name: String,
    entry: String,
    has_std_feature: bool,
}

/// Whether synchronization changed generated files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncStatus {
    /// Generated files were rewritten.
    Updated,
    /// Generated files were already current, or were only checked.
    Current,
}

/// A summary of the Plugins considered during synchronization.
#[derive(Debug, Eq, PartialEq)]
pub struct SyncReport {
    status: SyncStatus,
    enabled: Vec<String>,
    disabled: Vec<String>,
}

impl SyncReport {
    /// Returns whether generated files changed.
    pub const fn status(&self) -> SyncStatus {
        self.status
    }

    /// Returns enabled Plugin directory names in registry order.
    pub fn enabled(&self) -> &[String] {
        &self.enabled
    }

    /// Returns disabled Plugin directory names in name order.
    pub fn disabled(&self) -> &[String] {
        &self.disabled
    }
}

/// Failure while discovering, selecting, or synchronizing Plugins.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    /// A filesystem operation failed.
    #[error("failed to access `{path}`: {source}")]
    Io {
        /// Path of the failed operation.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },
    /// Discovered Plugin metadata is invalid.
    #[error("{0}")]
    Metadata(String),
    /// A generated block is malformed.
    #[error("{0}")]
    Generated(String),
    /// Generated registry files are stale.
    #[error("Plugin registry is stale; run `cargo plugin sync`")]
    Stale,
    /// Interactive selection failed.
    #[error("interactive Plugin selection failed: {0}")]
    Prompt(#[source] dialoguer::Error),
}

/// Synchronizes or validates the System registry against `plugins/`.
///
/// # Errors
/// Returns an error when discovery, generated files, or filesystem access fail.
pub fn sync(root: &Path, check: bool) -> Result<(), CommandError> {
    sync_with_report(root, check).map(|_| ())
}

/// Synchronizes the registry and returns the enabled and disabled Plugin sets.
///
/// # Errors
/// Returns an error when discovery, generated files, or filesystem access fail.
pub fn sync_with_report(root: &Path, check: bool) -> Result<SyncReport, CommandError> {
    let plugins = discover(root)?;
    let disabled_names = read_disabled(root)?;
    let (disabled, enabled): (Vec<_>, Vec<_>) = plugins
        .iter()
        .partition(|plugin| disabled_names.binary_search(&plugin.directory).is_ok());
    let system_manifest = root.join("core/system/Cargo.toml");
    let system_source = root.join("core/system/src/lib.rs");
    let old_manifest = read(&system_manifest)?;
    let old_source = read(&system_source)?;
    let manifest = replace_block(
        &old_manifest,
        MANIFEST_BEGIN,
        MANIFEST_END,
        &render_dependencies(&enabled),
    )?;
    let manifest = replace_block(
        &manifest,
        FEATURES_BEGIN,
        FEATURES_END,
        &render_features(&enabled),
    )?;
    let source = replace_block(
        &old_source,
        SOURCE_BEGIN,
        SOURCE_END,
        &render_registrations(&enabled),
    )?;
    let stale = manifest != old_manifest || source != old_source;
    if check && stale {
        return Err(CommandError::Stale);
    }
    if !check && stale {
        write(&system_manifest, &manifest)?;
        write(&system_source, &source)?;
    }
    Ok(SyncReport {
        status: if stale && !check {
            SyncStatus::Updated
        } else {
            SyncStatus::Current
        },
        enabled: enabled
            .iter()
            .map(|plugin| plugin.directory.clone())
            .collect(),
        disabled: disabled
            .iter()
            .map(|plugin| plugin.directory.clone())
            .collect(),
    })
}

/// Opens a multi-select prompt and persists the unchecked Plugins as disabled.
///
/// # Errors
/// Returns an error when discovery, prompting, or persistence fails.
pub fn configure(root: &Path) -> Result<(), CommandError> {
    let plugins = discover(root)?;
    let disabled = read_disabled(root)?;
    let names = plugins
        .iter()
        .map(|plugin| plugin.directory.as_str())
        .collect::<Vec<_>>();
    let defaults = plugins
        .iter()
        .map(|plugin| disabled.binary_search(&plugin.directory).is_err())
        .collect::<Vec<_>>();
    let selected = MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Select Plugins to enable — Space toggles, Enter saves")
        .items(&names)
        .defaults(&defaults)
        .interact()
        .map_err(CommandError::Prompt)?;
    let disabled = plugins
        .iter()
        .enumerate()
        .filter(|(index, _)| !selected.contains(index))
        .map(|(_, plugin)| plugin.directory.clone())
        .collect::<Vec<_>>();
    write_disabled(root, &disabled)
}

fn read_disabled(root: &Path) -> Result<Vec<String>, CommandError> {
    let path = root.join(DISABLED_PATH);
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(CommandError::Io { path, source }),
    };
    let mut names = contents
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(String::from)
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    Ok(names)
}

fn write_disabled(root: &Path, names: &[String]) -> Result<(), CommandError> {
    let path = root.join(DISABLED_PATH);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| CommandError::Io {
            path: parent.to_owned(),
            source,
        })?;
    }
    let contents = if names.is_empty() {
        String::new()
    } else {
        format!("{}\n", names.join("\n"))
    };
    write(&path, &contents)
}

fn discover(root: &Path) -> Result<Vec<Plugin>, CommandError> {
    let path = root.join("plugins");
    let entries = fs::read_dir(&path).map_err(|source| CommandError::Io {
        path: path.clone(),
        source,
    })?;
    let mut plugins = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| CommandError::Io {
            path: path.clone(),
            source,
        })?;
        if !entry
            .file_type()
            .map_err(|source| CommandError::Io {
                path: entry.path(),
                source,
            })?
            .is_dir()
        {
            continue;
        }
        let directory = entry.file_name().into_string().map_err(|name| {
            CommandError::Metadata(format!("Plugin directory is not valid UTF-8: {name:?}"))
        })?;
        let manifest_path = entry.path().join("crates/plugin/Cargo.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let package = package_name(&read(&manifest_path)?).ok_or_else(|| {
            CommandError::Metadata(format!(
                "missing package name in {}",
                manifest_path.display()
            ))
        })?;
        let source_path = entry.path().join("crates/plugin/src/lib.rs");
        let entry_name = plugin_entry(&read(&source_path)?).ok_or_else(|| {
            CommandError::Metadata(format!(
                "expected one Plugin implementation in {}",
                source_path.display()
            ))
        })?;
        plugins.push(Plugin {
            directory,
            crate_name: package.replace('-', "_"),
            package,
            entry: entry_name,
            has_std_feature: has_feature(&read(&manifest_path)?, "std"),
        });
    }
    plugins.sort_by(|left, right| left.directory.cmp(&right.directory));
    Ok(plugins)
}

fn read(path: &Path) -> Result<String, CommandError> {
    fs::read_to_string(path).map_err(|source| CommandError::Io {
        path: path.to_owned(),
        source,
    })
}
fn write(path: &Path, contents: &str) -> Result<(), CommandError> {
    fs::write(path, contents).map_err(|source| CommandError::Io {
        path: path.to_owned(),
        source,
    })
}

fn package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package {
            let value = line
                .strip_prefix("name")?
                .trim_start()
                .strip_prefix('=')?
                .trim();
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
        .filter_map(|rest| rest.split_whitespace().next())
        .filter(|entry| *entry != "Dependency")
        .map(String::from)
        .collect::<Vec<_>>();
    match entries.as_slice() {
        [entry] => Some(entry.clone()),
        _ => None,
    }
}

fn render_dependencies(plugins: &[&Plugin]) -> String {
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

fn render_features(plugins: &[&Plugin]) -> String {
    plugins
        .iter()
        .filter(|plugin| plugin.has_std_feature)
        .map(|plugin| format!("    \"{}/std\",", plugin.package))
        .collect::<Vec<_>>()
        .join("\n")
}

fn has_feature(manifest: &str, feature: &str) -> bool {
    let mut in_features = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_features = line == "[features]";
            continue;
        }
        if in_features
            && line
                .split_once('=')
                .is_some_and(|(name, _)| name.trim() == feature)
        {
            return true;
        }
    }
    false
}
fn render_registrations(plugins: &[&Plugin]) -> String {
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
fn replace_block(text: &str, begin: &str, end: &str, body: &str) -> Result<String, CommandError> {
    let begin_offset = text.find(begin).ok_or_else(|| {
        CommandError::Generated(format!("missing generated block marker `{begin}`"))
    })?;
    let line_start = text[..begin_offset]
        .rfind('\n')
        .map_or(0, |offset| offset + 1);
    let indentation = &text[line_start..begin_offset];
    if !indentation.bytes().all(|byte| matches!(byte, b' ' | b'\t')) {
        return Err(CommandError::Generated(format!(
            "invalid indentation before `{begin}`"
        )));
    }
    let end_offset = text[begin_offset..]
        .find(end)
        .map(|offset| begin_offset + offset)
        .ok_or_else(|| {
            CommandError::Generated(format!("missing generated block marker `{end}`"))
        })?;
    let line_end = text[end_offset..]
        .find('\n')
        .map_or(text.len(), |offset| end_offset + offset);
    Ok(format!(
        "{}{}{}\n{}\n{}{}{}",
        &text[..line_start],
        indentation,
        begin,
        body,
        indentation,
        end,
        &text[line_end..]
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::fs;

    use tempfile::tempdir;

    use super::{package_name, plugin_entry, replace_block, sync_with_report, SyncStatus};
    #[test]
    fn reads_plugin_metadata() {
        assert_eq!(
            package_name("[package]\nname = \"barracuda-demo-plugin\"\n"),
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

    #[test]
    fn disabled_plugins_are_reported_and_omitted_from_registration() {
        let root = tempdir().expect("temporary workspace");
        let plugin = root.path().join("plugins/demo/crates/plugin");
        fs::create_dir_all(plugin.join("src")).expect("Plugin source directory");
        fs::write(
            plugin.join("Cargo.toml"),
            "[package]\nname = \"barracuda-demo-plugin\"\n",
        )
        .expect("Plugin manifest");
        fs::write(
            plugin.join("src/lib.rs"),
            "impl<const M: usize> Plugin<M> for DemoPlugin {}\n",
        )
        .expect("Plugin source");
        fs::create_dir_all(root.path().join("core/system/src")).expect("System source directory");
        fs::write(
            root.path().join("core/system/Cargo.toml"),
            "[features]\ntokio = [\n# BEGIN GENERATED PLUGIN FEATURES\nold\n# END GENERATED PLUGIN FEATURES\n]\n# BEGIN GENERATED PLUGINS\nold\n# END GENERATED PLUGINS\n",
        )
        .expect("System manifest");
        fs::write(
            root.path().join("core/system/src/lib.rs"),
            "// BEGIN GENERATED PLUGINS\nold\n// END GENERATED PLUGINS\n",
        )
        .expect("System source");
        fs::create_dir_all(root.path().join(".barracuda")).expect("state directory");
        fs::write(root.path().join(".barracuda/disabled-plugins"), "demo\n")
            .expect("disabled Plugins");

        let report = sync_with_report(root.path(), false).expect("synchronize registry");

        assert_eq!(report.status(), SyncStatus::Updated);
        assert!(report.enabled().is_empty());
        assert_eq!(report.disabled(), ["demo"]);
        let source = fs::read_to_string(root.path().join("core/system/src/lib.rs"))
            .expect("generated System source");
        assert!(!source.contains("DemoPlugin::new"));
        let manifest = fs::read_to_string(root.path().join("core/system/Cargo.toml"))
            .expect("generated System manifest");
        assert!(!manifest.contains("barracuda-demo-plugin"));
    }
}
