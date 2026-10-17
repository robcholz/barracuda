//! The shapes every Plugin keeps, checked by `cargo plugin sync`, never by a
//! build: a document out of form fails the check, not the firmware.
//!
//! These are the checkable parts of `docs/architecture/plugin-api.mdx`: the Plugin
//! root's layout, the schema tree, the Plugin page summary and its agreement
//! with `plugin.toml`, the Action and Event pages, and crate dependencies on
//! other Plugins. Every violation is reported at once.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use barracuda_plugin::manifest::PluginManifest;

use crate::CommandError;

/// Names a Plugin root may hold.
const ROOT_ENTRIES: [&str; 5] = [
    "plugin.toml",
    "crates",
    "schemas",
    "resources",
    "filesystem",
];

/// The Plugin page summary bullets, in order.
const SUMMARY: [&str; 12] = [
    "Plugin ID",
    "Dependencies",
    "Settings",
    "Context resources",
    "Provided capabilities",
    "Required capabilities",
    "Workflow",
    "Agent Tools",
    "Lua packages",
    "Web",
    "Tasks",
    "Storage",
];

/// One Plugin as the checks see it.
pub(crate) struct Checked<'a> {
    pub(crate) directory: &'a str,
    pub(crate) manifest: &'a PluginManifest,
}

/// Checks every Plugin under `root/plugins`.
///
/// # Errors
/// Returns every violation in one [`CommandError::Metadata`].
pub(crate) fn check(root: &Path, plugins: &[Checked<'_>]) -> Result<(), CommandError> {
    let mut violations = Vec::new();
    let packages = plugin_packages(root, plugins)?;
    for plugin in plugins {
        let directory = root.join("plugins").join(plugin.directory);
        check_root(&directory, plugin.directory, &mut violations)?;
        check_schemas(&directory, plugin.directory, &mut violations)?;
        check_documents(root, &directory, plugin, &mut violations)?;
        check_crate_dependencies(&directory, plugin, plugins, &packages, &mut violations)?;
    }
    if violations.is_empty() {
        Ok(())
    } else {
        Err(CommandError::Metadata(format!(
            "Plugins break the Plugin API conventions:\n  {}",
            violations.join("\n  ")
        )))
    }
}

fn check_root(
    directory: &Path,
    name: &str,
    violations: &mut Vec<String>,
) -> Result<(), CommandError> {
    for entry in entries(directory)? {
        if !ROOT_ENTRIES.contains(&entry.as_str()) && !entry.starts_with('.') {
            violations.push(format!(
                "plugins/{name}/{entry} is not part of the Plugin layout"
            ));
        }
    }
    Ok(())
}

fn check_schemas(
    directory: &Path,
    name: &str,
    violations: &mut Vec<String>,
) -> Result<(), CommandError> {
    let schemas = directory.join("schemas");
    if !schemas.is_dir() {
        return Ok(());
    }
    for kind in entries(&schemas)? {
        let path = schemas.join(&kind);
        match kind.as_str() {
            "action" if path.is_dir() => {
                for address in entries(&path)? {
                    let action = path.join(&address);
                    let files: BTreeSet<String> = if action.is_dir() {
                        entries(&action)?.into_iter().collect()
                    } else {
                        BTreeSet::new()
                    };
                    let expected: BTreeSet<String> =
                        ["request.json", "response.json"].map(String::from).into();
                    if files != expected {
                        violations.push(format!(
                            "plugins/{name}/schemas/action/{address} must hold exactly request.json and response.json"
                        ));
                    }
                }
            }
            "event" if path.is_dir() => {
                for event in entries(&path)? {
                    if !event.ends_with(".json") || path.join(&event).is_dir() {
                        violations.push(format!(
                            "plugins/{name}/schemas/event/{event} is not an <event-id>.json schema"
                        ));
                    }
                }
            }
            _ => violations.push(format!(
                "plugins/{name}/schemas/{kind} is neither action/ nor event/"
            )),
        }
    }
    Ok(())
}

/// Checks the Plugin's pages in the docs site: `docs/plugins/<name>.mdx`, and
/// `docs/plugins/<name>/{action,event}.mdx` beside matching schemas.
fn check_documents(
    root: &Path,
    directory: &Path,
    plugin: &Checked<'_>,
    violations: &mut Vec<String>,
) -> Result<(), CommandError> {
    let name = plugin.directory;
    let pages = root.join("docs").join("plugins");
    for kind in ["action", "event"] {
        let path = directory.join("schemas").join(kind);
        if path.is_dir()
            && !entries(&path)?.is_empty()
            && !pages.join(name).join(format!("{kind}.mdx")).is_file()
        {
            violations.push(format!(
                "docs/plugins/{name}/{kind}.mdx is missing while plugins/{name}/schemas/{kind}/ exists"
            ));
        }
    }
    let path = pages.join(format!("{name}.mdx"));
    let Ok(text) = fs::read_to_string(&path) else {
        violations.push(format!("docs/plugins/{name}.mdx is missing"));
        return Ok(());
    };
    for problem in summary_problems(&text, plugin.manifest) {
        violations.push(format!("docs/plugins/{name}.mdx: {problem}"));
    }
    Ok(())
}

/// Returns what is wrong with a Plugin page summary.
pub(crate) fn summary_problems(text: &str, manifest: &PluginManifest) -> Vec<String> {
    let bullets = summary(text);
    let keys: Vec<&str> = bullets.iter().map(|(key, _)| key.as_str()).collect();
    if keys.len() < SUMMARY.len() || keys[..SUMMARY.len()] != SUMMARY {
        return vec![format!(
            "the summary must open with the bullets {} in that order, found {}",
            SUMMARY.join(", "),
            if keys.is_empty() {
                String::from("none")
            } else {
                keys.join(", ")
            }
        )];
    }
    let value = |key: &str| {
        bullets
            .iter()
            .find(|(name, _)| name == key)
            .map_or("", |(_, value)| value.as_str())
    };
    let mut problems = Vec::new();
    let id = code_spans(value("Plugin ID"));
    if id != [manifest.id()] {
        problems.push(format!("Plugin ID must be `{}`", manifest.id()));
    }
    let dependencies = list(value("Dependencies"));
    if dependencies != manifest.dependencies() {
        problems.push(format!(
            "Dependencies must be {}",
            listing(manifest.dependencies())
        ));
    }
    let settings: BTreeSet<String> = setting_names(value("Settings")).into_iter().collect();
    let declared: BTreeSet<String> = manifest.config.keys().cloned().collect();
    if settings != declared {
        problems.push(format!(
            "Settings must name {}",
            listing(&declared.into_iter().collect::<Vec<_>>())
        ));
    }
    for (key, value) in &bullets[..SUMMARY.len()] {
        if value.trim().is_empty() {
            problems.push(format!("{key} is empty; write `none`"));
        }
    }
    problems
}

/// The top-level `- Key: value` bullets before the first `##` heading, with
/// their continuation lines joined.
fn summary(text: &str) -> Vec<(String, String)> {
    let mut bullets: Vec<(String, String)> = Vec::new();
    let mut open = false;
    for line in text.lines() {
        if line.starts_with("## ") {
            break;
        }
        if let Some(rest) = line.strip_prefix("- ") {
            open = false;
            if let Some((key, value)) = rest.split_once(':') {
                bullets.push((key.trim().to_owned(), value.trim().to_owned()));
                open = true;
            }
        } else if open && line.starts_with(' ') && !line.trim().is_empty() {
            if let Some((_, value)) = bullets.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else {
            open = false;
        }
    }
    bullets
}

/// The contents of every `` `code` `` span.
fn code_spans(value: &str) -> Vec<String> {
    value
        .split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

/// A comma-separated list of code spans, or `none`.
fn list(value: &str) -> Vec<String> {
    if value.trim() == "none" {
        Vec::new()
    } else {
        code_spans(value)
    }
}

/// The names of `` `name` (`type`, default `value`) `` entries, or none.
fn setting_names(value: &str) -> Vec<String> {
    if value.trim() == "none" {
        return Vec::new();
    }
    value
        .split("),")
        .filter_map(|item| code_spans(item).into_iter().next())
        .collect()
}

fn listing(names: &[String]) -> String {
    if names.is_empty() {
        String::from("`none`")
    } else {
        names
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Maps each package under `plugins/*/crates/*` to its Plugin's ID.
fn plugin_packages(
    root: &Path,
    plugins: &[Checked<'_>],
) -> Result<BTreeMap<String, String>, CommandError> {
    let mut packages = BTreeMap::new();
    for plugin in plugins {
        let crates = root.join("plugins").join(plugin.directory).join("crates");
        if !crates.is_dir() {
            continue;
        }
        for krate in entries(&crates)? {
            let manifest = crates.join(krate).join("Cargo.toml");
            if let Ok(text) = fs::read_to_string(&manifest) {
                if let Some(package) = crate::package_name(&text) {
                    packages.insert(package, plugin.manifest.id().to_owned());
                }
            }
        }
    }
    Ok(packages)
}

fn check_crate_dependencies(
    directory: &Path,
    plugin: &Checked<'_>,
    plugins: &[Checked<'_>],
    packages: &BTreeMap<String, String>,
    violations: &mut Vec<String>,
) -> Result<(), CommandError> {
    let crates = directory.join("crates");
    if !crates.is_dir() {
        return Ok(());
    }
    let id = plugin.manifest.id();
    let direct = plugin.manifest.dependencies();
    let reachable = reachable(id, plugins);
    for krate in entries(&crates)? {
        let path = crates.join(&krate).join("Cargo.toml");
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let manifest: toml::Table = toml::from_str(&text).map_err(|error| {
            CommandError::Metadata(format!("invalid {}: {error}", path.display()))
        })?;
        for (section, dependencies) in dependency_tables(&manifest) {
            let test_only = section == "dev-dependencies";
            for package in dependencies.keys() {
                let Some(other) = packages.get(package) else {
                    continue;
                };
                if other == id {
                    continue;
                }
                let allowed = if test_only {
                    reachable.contains(other.as_str())
                } else {
                    direct.iter().any(|dependency| dependency == other)
                };
                if !allowed {
                    violations.push(format!(
                        "plugins/{}/crates/{krate} uses `{package}` in [{section}] but `{id}` does not depend on `{other}`{}",
                        plugin.directory,
                        if test_only { " directly or through its dependencies" } else { "" }
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Every dependency table of a crate manifest, target-specific ones included.
fn dependency_tables(manifest: &toml::Table) -> Vec<(&'static str, &toml::Table)> {
    let targets = manifest
        .get("target")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|targets| targets.values().filter_map(toml::Value::as_table));
    std::iter::once(manifest)
        .chain(targets)
        .flat_map(|table| {
            ["dependencies", "build-dependencies", "dev-dependencies"]
                .into_iter()
                .filter_map(|section| {
                    table
                        .get(section)
                        .and_then(toml::Value::as_table)
                        .map(|dependencies| (section, dependencies))
                })
        })
        .collect()
}

/// The Plugins `id` depends on, directly or through its dependencies.
fn reachable<'a>(id: &str, plugins: &'a [Checked<'_>]) -> BTreeSet<&'a str> {
    let mut found = BTreeSet::new();
    let mut pending = vec![id.to_owned()];
    while let Some(next) = pending.pop() {
        let Some(plugin) = plugins.iter().find(|plugin| plugin.manifest.id() == next) else {
            continue;
        };
        for dependency in plugin.manifest.dependencies() {
            if found.insert(dependency.as_str()) {
                pending.push(dependency.clone());
            }
        }
    }
    found
}

fn entries(directory: &Path) -> Result<Vec<String>, CommandError> {
    let mut names = Vec::new();
    for entry in fs::read_dir(directory).map_err(|source| CommandError::Io {
        path: directory.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| CommandError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn manifest(text: &str) -> PluginManifest {
        barracuda_plugin::manifest::parse(text).expect("Plugin manifest")
    }

    const DOCUMENT: &str = "---\ntitle: \"Demo Plugin\"\n---\n\nOwns the demo.\n\n\
        - Plugin ID: `demo`\n\
        - Dependencies: `webserver`, `http`\n\
        - Settings: `slots` (`usize`, default `1`)\n\
        - Context resources: `ip_stack`\n\
        - Provided capabilities: `Demo`\n\
        - Required capabilities: `WebServer` from `webserver`,\n  `ClientFactory` from `http`\n\
        - Workflow: none\n\
        - Agent Tools: none\n\
        - Lua packages: none\n\
        - Web: none\n\
        - Tasks: none\n\
        - Storage: none\n\n## More\n";

    const MANIFEST: &str = "id = \"demo\"\ndepends-on = [\"webserver\", \"http\"]\n\
        description = \"Demo.\"\n\n[config.slots]\ntype = \"usize\"\ndefault = 1\n\
        description = \"Slots.\"\n";

    #[test]
    fn a_summary_in_the_template_form_agrees_with_its_manifest() {
        assert!(summary_problems(DOCUMENT, &manifest(MANIFEST)).is_empty());
    }

    #[test]
    fn a_summary_must_match_the_manifest_and_keep_every_bullet() {
        let renamed = DOCUMENT.replace("`demo`", "`other`");
        let reordered = DOCUMENT.replace("`webserver`, `http`", "`http`, `webserver`");
        let unsettled = DOCUMENT.replace("`slots` (`usize`, default `1`)", "none");
        let missing = DOCUMENT.replace("- Lua packages: none\n", "");
        let blank = DOCUMENT.replace("- Tasks: none", "- Tasks:");
        let problems = |text: &str| summary_problems(text, &manifest(MANIFEST));

        assert_eq!(problems(&renamed), ["Plugin ID must be `demo`"]);
        assert_eq!(
            problems(&reordered),
            ["Dependencies must be `webserver`, `http`"]
        );
        assert_eq!(problems(&unsettled), ["Settings must name `slots`"]);
        assert!(problems(&missing)[0].starts_with("the summary must open with"));
        assert_eq!(problems(&blank), ["Tasks is empty; write `none`"]);
    }
}
