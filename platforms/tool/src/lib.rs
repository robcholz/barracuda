//! Platform discovery and workspace registry synchronization.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use barracuda_platform_config::{discover_platforms, ResolveError};

const WORKSPACE_BEGIN: &str = "# BEGIN GENERATED PLATFORM WORKSPACE DEPENDENCIES";
const WORKSPACE_END: &str = "# END GENERATED PLATFORM WORKSPACE DEPENDENCIES";

/// Whether synchronization changed the generated registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncStatus {
    /// The generated registry was rewritten.
    Updated,
    /// The generated registry was already current, or was only checked.
    Current,
}

/// Result of synchronizing the Platform registry.
#[derive(Debug, Eq, PartialEq)]
pub struct SyncReport {
    status: SyncStatus,
    platforms: Vec<String>,
}

impl SyncReport {
    /// Returns whether generated source changed.
    #[must_use]
    pub const fn status(&self) -> SyncStatus {
        self.status
    }

    /// Returns discovered Platform names in registry order.
    #[must_use]
    pub fn platforms(&self) -> &[String] {
        &self.platforms
    }
}

/// Failure while synchronizing the Platform registry.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    /// Platform discovery or validation failed.
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    /// A generated block is malformed.
    #[error("{0}")]
    Generated(String),
    /// A registry source file could not be accessed.
    #[error("failed to access `{path}`: {source}")]
    Io {
        /// Path that could not be accessed.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },
    /// Generated Platform dependencies do not match the catalog.
    #[error("Platform registry is stale; run `cargo platform sync`")]
    Stale,
}

/// Synchronizes or validates the tracked Platform registry.
///
/// # Errors
///
/// Returns an error when discovery, generated markers, or file access fails.
pub fn sync(root: &Path, check: bool) -> Result<(), CommandError> {
    sync_with_report(root, check).map(|_report| ())
}

/// Synchronizes all discovered Platforms into the tracked workspace registry.
///
/// # Errors
///
/// Returns an error when discovery, generated markers, or file access fails.
pub fn sync_with_report(root: &Path, check: bool) -> Result<SyncReport, CommandError> {
    let platforms = discover_platforms(root)?;
    let manifest_path = root.join("Cargo.toml");
    let old_manifest = read(&manifest_path)?;
    let dependencies = platforms
        .iter()
        .map(|platform| {
            let path = platform
                .directory()
                .strip_prefix(root)
                .unwrap_or_else(|_error| platform.directory());
            format!(
                "{} = {{ path = {:?} }}",
                platform.package(),
                path.to_string_lossy()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let manifest = replace_block(&old_manifest, WORKSPACE_BEGIN, WORKSPACE_END, &dependencies)?;
    let stale = manifest != old_manifest;
    if check && stale {
        return Err(CommandError::Stale);
    }
    if stale {
        fs::write(&manifest_path, manifest).map_err(|source| CommandError::Io {
            path: manifest_path,
            source,
        })?;
    }
    Ok(SyncReport {
        status: if stale {
            SyncStatus::Updated
        } else {
            SyncStatus::Current
        },
        platforms: platforms
            .into_iter()
            .map(|platform| platform.name().to_owned())
            .collect(),
    })
}

fn read(path: &Path) -> Result<String, CommandError> {
    fs::read_to_string(path).map_err(|source| CommandError::Io {
        path: path.to_owned(),
        source,
    })
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

    use std::{fs, path::Path};

    use tempfile::tempdir;

    use super::{sync_with_report, CommandError, SyncStatus};

    fn add_platform(root: &Path, name: &str, package: &str) {
        let directory = root.join("platforms").join(name);
        fs::create_dir_all(&directory).expect("Platform directory");
        fs::write(
            directory.join("platform.yml"),
            format!(
                "name: {name}\npackage: {package}\ncrate: {}\ntype: TestPlatform\nselection:\n  board-chips: [{name}]\n  targets:\n    - os: {name}\nsystem-image:\n  layout:\n    driver: file-regions\n  flash:\n    driver: file\n    state-directory: .state\n    flash-image: flash.bin\n",
                package.replace('-', "_")
            ),
        )
        .expect("Platform manifest");
    }

    fn add_workspace(root: &Path) {
        fs::write(
            root.join("Cargo.toml"),
            "[workspace.dependencies]\n\
             # BEGIN GENERATED PLATFORM WORKSPACE DEPENDENCIES\n\
             old\n\
             # END GENERATED PLATFORM WORKSPACE DEPENDENCIES\n",
        )
        .expect("workspace manifest");
    }

    #[test]
    fn sync_registers_discovered_platforms_in_name_order() {
        let root = tempdir().expect("temporary workspace");
        add_workspace(root.path());
        add_platform(root.path(), "zeta", "barracuda-platform-zeta");
        add_platform(root.path(), "alpha", "barracuda-platform-alpha");

        let report = sync_with_report(root.path(), false).expect("Platform sync");

        assert_eq!(report.status(), SyncStatus::Updated);
        assert_eq!(report.platforms(), ["alpha", "zeta"]);
        let workspace =
            fs::read_to_string(root.path().join("Cargo.toml")).expect("workspace manifest");
        let alpha = workspace
            .find("barracuda-platform-alpha = { path = \"platforms/alpha\" }")
            .expect("alpha dependency");
        let zeta = workspace
            .find("barracuda-platform-zeta = { path = \"platforms/zeta\" }")
            .expect("zeta dependency");
        assert!(alpha < zeta);
        assert!(!workspace.contains("\nold\n"));
    }

    #[test]
    fn check_rejects_a_stale_registry_without_writing_it() {
        let root = tempdir().expect("temporary workspace");
        add_workspace(root.path());
        add_platform(root.path(), "alpha", "barracuda-platform-alpha");
        let before = fs::read_to_string(root.path().join("Cargo.toml")).expect("before");

        let error = sync_with_report(root.path(), true).expect_err("stale registry");

        assert!(matches!(error, CommandError::Stale));
        assert_eq!(
            fs::read_to_string(root.path().join("Cargo.toml")).expect("after"),
            before
        );
    }
}
