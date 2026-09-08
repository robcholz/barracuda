//! Host-only Plugin task execution shared by Cargo build scripts and the CLI.

use barracuda_plugin_manifest::{parse, PluginManifest, PluginTask};
use sha2::{Digest, Sha256};
use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, BufRead},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// A task could not be prepared, executed, or validated.
#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    /// Task configuration or execution failed, with Plugin/task context.
    #[error("{0}")]
    Task(String),
}

fn load(root: &Path) -> Result<PluginManifest, TaskError> {
    let path = root.join("plugin.toml");
    let text = fs::read_to_string(&path)
        .map_err(|error| TaskError::Task(format!("{}: {error}", path.display())))?;
    parse(&text).map_err(|error| TaskError::Task(format!("{}: {error}", path.display())))
}

/// Runs one task unconditionally. Returns false when the Plugin has no such task.
///
/// # Errors
/// Reports invalid manifests, unavailable commands, failed exits and missing outputs.
pub fn run(root: &Path, name: &str) -> Result<bool, TaskError> {
    let manifest = load(root)?;
    let Some(task) = manifest.tasks.get(name) else {
        return Ok(false);
    };
    execute(root, manifest.id(), name, task)?;
    Ok(true)
}

fn execute(root: &Path, id: &str, name: &str, task: &PluginTask) -> Result<(), TaskError> {
    let _lock = lock(root)?;
    command(root, id, name, task)
}

fn lock(root: &Path) -> Result<fs::File, TaskError> {
    let fail = |error: io::Error| TaskError::Task(format!("{}: {error}", root.display()));
    // Serialize manual tasks and Cargo hooks for the same Plugin across target dirs.
    let state = root.join(".barracuda");
    fs::create_dir_all(&state).map_err(fail)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state.join("tasks.lock"))
        .map_err(fail)?;
    lock.lock().map_err(fail)?;
    Ok(lock)
}

fn command(root: &Path, id: &str, name: &str, task: &PluginTask) -> Result<(), TaskError> {
    let fail = |message: String| TaskError::Task(format!("[{id}:{name}] {message}"));
    eprintln!(
        "[{id}:{name}] running {} in {}",
        task.command[0],
        root.join(&task.cwd).display()
    );
    let mut child = Command::new(&task.command[0])
        .args(&task.command[1..])
        .current_dir(root.join(&task.cwd))
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| {
            fail(format!(
                "cannot start `{}`: {error}; install the required tool and check PATH",
                task.command[0]
            ))
        })?;
    // Forward stdout to stderr so tool output cannot become Cargo build directives.
    if let Some(stdout) = child.stdout.take() {
        for line in io::BufReader::new(stdout).split(b'\n') {
            match line {
                Ok(line) => eprintln!("[{id}:{name}] {}", String::from_utf8_lossy(&line)),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(fail(error.to_string()));
                }
            }
        }
    }
    let status = child.wait().map_err(|error| fail(error.to_string()))?;
    if !status.success() {
        return Err(fail(format!("command failed: {status}")));
    }
    for output in &task.outputs {
        if !root.join(output).exists() {
            return Err(fail(format!(
                "command succeeded but output `{output}` is missing"
            )));
        }
    }
    Ok(())
}

/// Runs this Plugin's `build` task and declares Cargo's incremental inputs.
/// Call from the Plugin crate's `build.rs`; other task names are CLI-only.
///
/// # Errors
/// Reports missing Cargo context, invalid manifests, or task failures.
pub fn build() -> Result<(), TaskError> {
    let output_timestamp = SystemTime::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or(UNIX_EPOCH);
    let directory = env::var_os("CARGO_MANIFEST_DIR")
        .ok_or_else(|| TaskError::Task("missing CARGO_MANIFEST_DIR".into()))?;
    let directory = PathBuf::from(directory);
    let root = directory
        .ancestors()
        .find(|path| path.join("plugin.toml").is_file())
        .ok_or_else(|| TaskError::Task("build hook must be inside a Plugin directory".into()))?;
    let manifest = load(root)?;
    println!(
        "cargo:rerun-if-changed={}",
        root.join("plugin.toml").display()
    );
    let Some(task) = manifest.tasks.get("build") else {
        return Ok(());
    };
    println!("cargo:rerun-if-env-changed=PATH");
    for variable in &task.env {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    if task.inputs.is_empty() {
        println!("cargo:rerun-if-changed={}", root.join(&task.cwd).display());
    }
    for path in &task.inputs {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    let _lock = lock(root)?;
    if task.inputs.is_empty() || task.outputs.is_empty() {
        command(root, manifest.id(), "build", task)?;
        backdate_outputs(root, &task.outputs, output_timestamp)?;
    } else {
        let cache = PathBuf::from(
            env::var_os("OUT_DIR").ok_or_else(|| TaskError::Task("missing OUT_DIR".into()))?,
        )
        .join("plugin-build-fingerprint");
        let mut inputs = Sha256::new();
        hash_path(&mut inputs, &root.join("plugin.toml"))?;
        for path in &task.inputs {
            hash_path(&mut inputs, &root.join(path))?;
        }
        for variable in std::iter::once("PATH").chain(task.env.iter().map(String::as_str)) {
            inputs.update(format!("{variable}={:?}\n", env::var_os(variable)));
        }
        let inputs = inputs.finalize();
        let outputs_exist = task.outputs.iter().all(|path| root.join(path).exists());
        if !outputs_exist
            || fs::read(&cache).ok().as_deref()
                != Some(fingerprint(&inputs, root, &task.outputs)?.as_slice())
        {
            command(root, manifest.id(), "build", task)?;
            backdate_outputs(root, &task.outputs, output_timestamp)?;
            fs::write(&cache, fingerprint(&inputs, root, &task.outputs)?)
                .map_err(|error| TaskError::Task(format!("{}: {error}", cache.display())))?;
        }
    }

    // Declare outputs only after a successful task has validated and dated
    // them for Cargo's next fingerprint check.
    for path in &task.outputs {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    Ok(())
}

// Cargo records its build-script reference timestamp before the task can write
// source-tree outputs. Date successful outputs just before this invocation so
// watching them does not immediately invalidate the build that produced them.
fn backdate_outputs(
    root: &Path,
    outputs: &[String],
    modified: SystemTime,
) -> Result<(), TaskError> {
    for output in outputs {
        set_modified(&root.join(output), modified)?;
    }
    Ok(())
}

fn set_modified(path: &Path, modified: SystemTime) -> Result<(), TaskError> {
    let fail = |error: io::Error| TaskError::Task(format!("{}: {error}", path.display()));
    let metadata = fs::symlink_metadata(path).map_err(fail)?;
    if metadata.file_type().is_symlink() {
        return Err(TaskError::Task(format!(
            "task output must not be a symlink: {}",
            path.display()
        )));
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(fail)? {
            set_modified(&entry.map_err(fail)?.path(), modified)?;
        }
    }
    fs::File::open(path)
        .and_then(|file| file.set_times(fs::FileTimes::new().set_modified(modified)))
        .map_err(fail)
}

fn fingerprint(inputs: &[u8], root: &Path, outputs: &[String]) -> Result<Vec<u8>, TaskError> {
    let mut hash = Sha256::new();
    hash.update(inputs);
    for path in outputs {
        hash_path(&mut hash, &root.join(path))?;
    }
    Ok(hash.finalize().to_vec())
}

fn hash_path(hash: &mut Sha256, path: &Path) -> Result<(), TaskError> {
    let fail = |error: io::Error| TaskError::Task(format!("{}: {error}", path.display()));
    hash.update(format!("{:?}\0", path));
    let metadata = fs::symlink_metadata(path).map_err(fail)?;
    if metadata.file_type().is_symlink() {
        return Err(TaskError::Task(format!(
            "task input/output must not be a symlink: {}",
            path.display()
        )));
    }
    if metadata.is_dir() {
        hash.update(b"directory\0");
        let mut children = fs::read_dir(path)
            .map_err(fail)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(fail)?;
        children.sort();
        for child in children {
            hash_path(hash, &child)?;
        }
    } else if metadata.is_file() {
        hash.update(metadata.len().to_le_bytes());
        let mut file = fs::File::open(path).map_err(fail)?;
        let mut buffer = [0; 8192];
        loop {
            let count = io::Read::read(&mut file, &mut buffer).map_err(fail)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
    } else {
        return Err(TaskError::Task(format!(
            "task input/output must be a regular file or directory: {}",
            path.display()
        )));
    }
    Ok(())
}
