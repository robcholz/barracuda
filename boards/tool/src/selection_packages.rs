//! Generates ignored Cargo packages that carry one local Board selection.

use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};

const PLATFORM_SELECTION_DIRECTORY: &str = ".barracuda/selection/platform";
const BOARD_SELECTION_DIRECTORY: &str = ".barracuda/selection/board";

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Package {
    name: String,
    crate_name: String,
    path: PathBuf,
}

#[allow(dead_code)]
/// Ensures the local selection packages exist before Cargo resolves the workspace.
pub fn ensure(workspace_root: &Path) -> io::Result<()> {
    let platform_manifest = workspace_root
        .join(PLATFORM_SELECTION_DIRECTORY)
        .join("Cargo.toml");
    let board_manifest = workspace_root
        .join(BOARD_SELECTION_DIRECTORY)
        .join("Cargo.toml");
    let platform_features = read_default_features(&platform_manifest)?;
    let board_features = read_default_features(&board_manifest)?;
    write_features(workspace_root, &platform_features, &board_features, false)
}

/// Writes the concrete Platform and peripheral dependencies for one selection.
pub fn write(
    workspace_root: &Path,
    selected_platform: Option<&str>,
    platform_features: &[String],
    selected_board_packages: &[String],
) -> io::Result<()> {
    write_features(
        workspace_root,
        &selected_features(selected_platform, platform_features),
        &selected_features(selected_board_packages.iter().map(String::as_str), &[]),
        true,
    )
}

fn write_features(
    workspace_root: &Path,
    platform_features: &[String],
    board_features: &[String],
    strict: bool,
) -> io::Result<()> {
    let platforms = discover_packages(
        workspace_root,
        &workspace_root.join("platforms"),
        "platform.yml",
    )?;
    let peripherals = discover_packages(
        workspace_root,
        &workspace_root.join("peripherals/impl"),
        "peripheral.yml",
    )?;

    write_package(
        &workspace_root.join(PLATFORM_SELECTION_DIRECTORY),
        "barracuda-platform-selection",
        &platforms,
        platform_features,
        strict,
        Reexport::Contents,
    )?;
    write_package(
        &workspace_root.join(BOARD_SELECTION_DIRECTORY),
        "barracuda-board-selection",
        &peripherals,
        board_features,
        strict,
        Reexport::Crate,
    )
}

fn read_default_features(manifest: &Path) -> io::Result<Vec<String>> {
    let contents = match fs::read_to_string(manifest) {
        Ok(contents) => contents,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(source),
    };
    let Some(default) = contents
        .lines()
        .find_map(|line| line.trim().strip_prefix("default = ["))
        .and_then(|line| line.strip_suffix(']'))
    else {
        return Ok(Vec::new());
    };
    Ok(default
        .split(',')
        .map(str::trim)
        .map(|feature| feature.trim_matches('"'))
        .filter(|feature| !feature.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn selected_features<'a>(
    packages: impl IntoIterator<Item = &'a str>,
    dependency_features: &[String],
) -> Vec<String> {
    let mut selected = packages
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if let Some(package) = selected.first().cloned() {
        selected.extend(
            dependency_features
                .iter()
                .map(|feature| format!("{package}/{feature}")),
        );
    }
    selected
}

fn discover_packages(
    workspace_root: &Path,
    directory: &Path,
    marker: &str,
) -> io::Result<Vec<Package>> {
    let mut pending = vec![directory.to_owned()];
    let mut packages = BTreeSet::new();
    while let Some(directory) = pending.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(source),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path();
            if path.join(marker).is_file() {
                let manifest = path.join("Cargo.toml");
                let name = read_package_name(&manifest)?;
                let relative = path.strip_prefix(workspace_root).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("package path `{}` is outside the workspace", path.display()),
                    )
                })?;
                packages.insert(Package {
                    crate_name: name.replace('-', "_"),
                    name,
                    path: relative.to_owned(),
                });
            } else {
                pending.push(path);
            }
        }
    }
    Ok(packages.into_iter().collect())
}

fn read_package_name(manifest: &Path) -> io::Result<String> {
    let contents = fs::read_to_string(manifest)?;
    let mut in_package = false;
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some(value) = line
            .strip_prefix("name")
            .and_then(|line| line.trim().strip_prefix('='))
        else {
            continue;
        };
        let name = value.trim().trim_matches('"');
        if !name.is_empty() {
            return Ok(name.to_owned());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "Cargo manifest `{}` has no package name",
            manifest.display()
        ),
    ))
}

#[derive(Clone, Copy)]
enum Reexport {
    Contents,
    Crate,
}

fn write_package(
    directory: &Path,
    package_name: &str,
    packages: &[Package],
    selected: &[String],
    strict: bool,
    reexport: Reexport,
) -> io::Result<()> {
    let mut available = Vec::new();
    for feature in selected {
        let package = feature
            .split_once('/')
            .map_or(feature.as_str(), |(package, _)| package);
        if packages.iter().any(|candidate| candidate.name == package) {
            available.push(feature.clone());
        } else if strict {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("selected package `{package}` is absent from the source catalog"),
            ));
        }
    }

    let default = available
        .iter()
        .map(|feature| format!("{feature:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut manifest = format!(
        "# Generated by `cargo board`; do not edit.\n\n[package]\nname = {package_name:?}\nversion = \"0.0.0\"\nedition = \"2021\"\npublish = false\n\n[features]\ndefault = [{default}]\n"
    );
    for package in packages {
        manifest.push_str(&format!("{} = [\"dep:{}\"]\n", package.name, package.name));
    }
    manifest.push_str("\n[dependencies]\n");
    for package in packages {
        manifest.push_str(&format!(
            "{} = {{ path = {:?}, optional = true }}\n",
            package.name,
            Path::new("../../..").join(&package.path).to_string_lossy()
        ));
    }

    let mut source = String::from(
        "//! Generated dependency boundary for one Board selection.\n\n#![no_std]\n\n",
    );
    for package in packages {
        source.push_str(&format!("#[cfg(feature = {:?})]\n", package.name));
        match reexport {
            Reexport::Contents => {
                source.push_str(&format!("pub use {}::*;\n", package.crate_name));
            }
            Reexport::Crate => {
                source.push_str(&format!("pub use {};\n", package.crate_name));
            }
        }
    }

    write_if_changed(&directory.join("Cargo.toml"), &manifest)?;
    write_if_changed(&directory.join("src/lib.rs"), &source)
}

fn write_if_changed(path: &Path, contents: &str) -> io::Result<()> {
    if fs::read_to_string(path).is_ok_and(|current| current == contents) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)
}
