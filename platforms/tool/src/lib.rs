//! Platform discovery and workspace registry synchronization.

use std::{
    env,
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
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
    /// A requested Platform is not registered.
    #[error("unknown Platform `{0}`")]
    UnknownPlatform(String),
    /// A Platform launcher references a support binary it did not declare.
    #[error("launcher references undeclared support binary `{0}`")]
    UndeclaredSupportBinary(String),
    /// A child build or launcher command could not be started.
    #[error("failed to start {kind}: {source}")]
    Process {
        /// Operation being started.
        kind: &'static str,
        /// Underlying process error.
        #[source]
        source: io::Error,
    },
    /// A child build command exited unsuccessfully.
    #[error("{kind} exited with {status}")]
    ProcessFailed {
        /// Operation that failed.
        kind: &'static str,
        /// Unsuccessful process status.
        status: ExitStatus,
    },
    /// The Rust compiler did not report a usable host target tuple.
    #[error("Rust host target tuple is unavailable")]
    HostTupleUnavailable,
}

/// Builds Platform support binaries and invokes its application launcher.
///
/// # Errors
///
/// Returns an error when the Platform is unknown, a support build fails, or the
/// configured launcher cannot be started.
pub fn launch(
    root: &Path,
    platform_name: &str,
    application: &Path,
    application_arguments: &[OsString],
) -> Result<ExitStatus, CommandError> {
    let platform = discover_platforms(root)?
        .into_iter()
        .find(|platform| platform.name() == platform_name)
        .ok_or_else(|| CommandError::UnknownPlatform(platform_name.to_owned()))?;
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    for binary in platform.application().support_binaries() {
        let mut command = Command::new(&cargo);
        command
            .args(["build", "--target", "host-tuple", "--package"])
            .arg(platform.package())
            .args(["--bin", binary])
            .current_dir(root);
        successful(&mut command, "Platform support binary build")?;
    }

    let Some(launcher) = platform.application().launcher() else {
        return Command::new(application)
            .args(application_arguments)
            .current_dir(root)
            .status()
            .map_err(|source| CommandError::Process {
                kind: "application",
                source,
            });
    };
    let target_directory = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map_or_else(
            || root.join("target"),
            |path| {
                if path.is_absolute() {
                    path
                } else {
                    root.join(path)
                }
            },
        );
    let host = host_tuple(root)?;
    let binary_directory = target_directory.join(host).join("debug");
    let program = expand_launcher_argument(
        &launcher.program().to_string_lossy(),
        root,
        &target_directory,
        application,
        &binary_directory,
        platform.application().support_binaries(),
    )?;
    let program = resolve_program(Path::new(&program), platform.directory());
    let mut command = Command::new(program);
    for argument in launcher.arguments() {
        command.arg(expand_launcher_argument(
            argument,
            root,
            &target_directory,
            application,
            &binary_directory,
            platform.application().support_binaries(),
        )?);
    }
    command.args(application_arguments).current_dir(root);
    command.status().map_err(|source| CommandError::Process {
        kind: "Platform application launcher",
        source,
    })
}

fn successful(command: &mut Command, kind: &'static str) -> Result<(), CommandError> {
    let status = command
        .status()
        .map_err(|source| CommandError::Process { kind, source })?;
    if status.success() {
        Ok(())
    } else {
        Err(CommandError::ProcessFailed { kind, status })
    }
}

/// Returns the Rust compiler's concrete host target tuple.
///
/// # Errors
///
/// Returns an error when the compiler cannot be started or reports no tuple.
pub fn host_tuple(root: &Path) -> Result<String, CommandError> {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
    let output = Command::new(rustc)
        .args(["--print", "host-tuple"])
        .current_dir(root)
        .output()
        .map_err(|source| CommandError::Process {
            kind: "Rust host target query",
            source,
        })?;
    if !output.status.success() {
        return Err(CommandError::ProcessFailed {
            kind: "Rust host target query",
            status: output.status,
        });
    }
    let host = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if host.is_empty() {
        Err(CommandError::HostTupleUnavailable)
    } else {
        Ok(host)
    }
}

fn resolve_program(program: &Path, platform: &Path) -> PathBuf {
    if program.components().count() == 1 || program.is_absolute() {
        program.to_owned()
    } else {
        platform.join(program)
    }
}

fn expand_launcher_argument(
    argument: &str,
    workspace: &Path,
    target_directory: &Path,
    application: &Path,
    binary_directory: &Path,
    support_binaries: &[String],
) -> Result<OsString, CommandError> {
    if argument == "{application}" {
        return Ok(application.as_os_str().to_owned());
    }
    if argument == "{workspace}" {
        return Ok(workspace.as_os_str().to_owned());
    }
    if argument == "{target-dir}" {
        return Ok(target_directory.as_os_str().to_owned());
    }
    if let Some(binary) = argument
        .strip_prefix("{support:")
        .and_then(|value| value.strip_suffix('}'))
    {
        if support_binaries.iter().any(|candidate| candidate == binary) {
            return Ok(binary_directory
                .join(format!("{binary}{}", env::consts::EXE_SUFFIX))
                .into_os_string());
        }
        return Err(CommandError::UndeclaredSupportBinary(binary.to_owned()));
    }
    Ok(OsStr::new(argument).to_owned())
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

    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use tempfile::tempdir;

    use super::{expand_launcher_argument, sync_with_report, CommandError, SyncStatus};

    fn add_platform(root: &Path, name: &str, package: &str) {
        let directory = root.join("platforms").join(name);
        fs::create_dir_all(&directory).expect("Platform directory");
        fs::write(
            directory.join("platform.yml"),
            format!(
                "name: {name}\ninfo:\n  family: test\n  environment: hosted\npackage: {package}\ncrate: {}\ntype: TestPlatform\nselection:\n  board-chips: [{name}]\n  targets:\n    - os: {name}\nsystem-image:\n  layout:\n    driver: file-regions\n  flash:\n    driver: file\n    state-directory: .state\n    flash-image: flash.bin\n",
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

    #[test]
    fn launcher_expands_only_declared_support_binaries() {
        let support = [String::from("network-helper")];
        let expanded = expand_launcher_argument(
            "{support:network-helper}",
            Path::new("/workspace"),
            Path::new("/workspace/target"),
            Path::new("/workspace/target/debug/app"),
            Path::new("/workspace/target/host/debug"),
            &support,
        )
        .expect("declared support binary");

        assert_eq!(
            PathBuf::from(expanded),
            PathBuf::from("/workspace/target/host/debug/network-helper")
        );

        let error = expand_launcher_argument(
            "{support:missing}",
            Path::new("/workspace"),
            Path::new("/workspace/target"),
            Path::new("/workspace/target/debug/app"),
            Path::new("/workspace/target/host/debug"),
            &support,
        )
        .expect_err("undeclared support binary");
        assert!(matches!(error, CommandError::UndeclaredSupportBinary(name) if name == "missing"));
    }

    #[test]
    fn launcher_program_can_be_a_declared_support_binary() {
        let support = [String::from("macos-launcher")];
        let expanded = expand_launcher_argument(
            "{support:macos-launcher}",
            Path::new("/workspace"),
            Path::new("/workspace/target"),
            Path::new("/workspace/target/debug/app"),
            Path::new("/workspace/target/host/debug"),
            &support,
        )
        .expect("declared support binary");

        assert_eq!(
            PathBuf::from(expanded),
            PathBuf::from("/workspace/target/host/debug/macos-launcher")
        );
    }

    #[test]
    fn platform_without_launcher_executes_the_application_directly() {
        let root = tempdir().expect("temporary workspace");
        add_platform(root.path(), "direct", "barracuda-platform-direct");

        let status = super::launch(root.path(), "direct", Path::new("/usr/bin/true"), &[])
            .expect("direct application launch");

        assert!(status.success());
    }
}
