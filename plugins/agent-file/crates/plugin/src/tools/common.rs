use alloc::string::{String, ToString};

use barracuda_agent_plugin::tools::{
    Action, Resource, RiskClass, ToolError, ToolInvocation, ToolInvokeError, ToolOutput,
};
use barracuda_vfs::{FileType, FsError};
use serde::{Deserialize, Serialize};

pub(super) const MAX_WRITE_BYTES: usize = 32 * 1024;

#[derive(Deserialize)]
struct PathField {
    path: String,
}

pub(super) fn path_action(call: &ToolInvocation, verb: &'static str, risk: RiskClass) -> Action {
    let action = Action::new(verb, risk);
    match call.arguments::<PathField>() {
        Ok(args) => action.with_resource(Resource::Path(args.path)),
        Err(_error) => action,
    }
}

pub(super) fn file_kind(file_type: FileType) -> &'static str {
    match file_type {
        FileType::File => "file",
        FileType::Directory => "directory",
        FileType::Symlink => "symlink",
        FileType::Other => "other",
    }
}

pub(super) fn encode_success(response: &impl Serialize) -> Result<ToolOutput, ToolInvokeError> {
    encode(response, true)
}

pub(super) fn encode_failure(failure: Failure) -> Result<ToolOutput, ToolInvokeError> {
    encode(&failure, false)
}

pub(super) fn encode_fs_failure(error: FsError) -> Result<ToolOutput, ToolInvokeError> {
    encode_failure(Failure::new(fs_error_code(error), error.to_string()))
}

fn encode(response: &impl Serialize, ok: bool) -> Result<ToolOutput, ToolInvokeError> {
    let content = serde_json::to_string(response).map_err(|_error| {
        ToolError::InvokeRejected(String::from("failed to encode file Tool response"))
    })?;
    Ok(ToolOutput { content, ok })
}

fn fs_error_code(error: FsError) -> &'static str {
    match error {
        FsError::NotFound => "not_found",
        FsError::AlreadyExists => "already_exists",
        FsError::PermissionDenied => "permission_denied",
        FsError::ReadOnly => "read_only",
        FsError::InvalidPath => "invalid_path",
        FsError::NotMounted => "not_mounted",
        FsError::MountConflict => "mount_conflict",
        FsError::CrossMount => "cross_mount",
        FsError::Busy => "busy",
        FsError::IsDirectory => "is_directory",
        FsError::NotDirectory => "not_directory",
        FsError::DirectoryNotEmpty => "directory_not_empty",
        FsError::Unsupported => "unsupported",
        FsError::InvalidInput => "invalid_input",
        FsError::Io => "io",
        _ => "filesystem",
    }
}

#[derive(Serialize)]
pub(super) struct Failure {
    error: &'static str,
    message: String,
}

impl Failure {
    pub(super) fn new(error: &'static str, message: impl Into<String>) -> Self {
        Self {
            error,
            message: message.into(),
        }
    }
}

#[cfg(test)]
pub(super) async fn filesystem()
-> Result<barracuda_vfs::ScopedVfs, alloc::boxed::Box<dyn core::error::Error>> {
    use barracuda_vfs::{MountOptions, Vfs};
    use barracuda_vfs_memfs::MemFs;

    let vfs = Vfs::new();
    vfs.mount("/", MemFs::new().into_backend(), MountOptions::read_write())
        .await?;
    Ok(vfs.scoped("/workspace")?)
}

#[cfg(test)]
pub(super) fn json(content: &str) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::from_str(content)
}
