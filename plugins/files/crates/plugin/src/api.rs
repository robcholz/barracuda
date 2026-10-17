//! The `/api/files` routes over the Plugin's inspecting filesystem view.
//!
//! Paths are the view's real paths. Reads take them percent-encoded in the URL
//! path; changes take them as JSON strings. Which trees may change is decided
//! by the view itself: a change beneath a read-only mount fails there and is
//! answered 403.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use barracuda_vfs::{File, FsError, OpenOptions, ScopedVfs};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpProvider, HttpRequest, HttpResponse, HttpUpload,
    HttpUploadEndpoint, WebRouteRegistration, WebServer, WebServerError,
};
use embedded_io_async::{Read, Write};
use serde::{Deserialize, Serialize};

use crate::path;

const LIST: &str = "/api/files/list/";
const RAW: &str = "/api/files/raw/";
const UPLOAD: &str = "/api/files/upload/";
/// A listing names at most this many entries, directories first.
const MAX_ENTRIES: usize = 256;
const COPY_CHUNK: usize = 1024;
const JSON: &str = "application/json";

/// Registers every route; dropping the registrations removes them.
pub(crate) fn register(
    webserver: &WebServer,
    filesystem: &ScopedVfs,
) -> Result<[WebRouteRegistration; 6], WebServerError> {
    let change = |change| ChangeEndpoint {
        filesystem: filesystem.clone(),
        change,
    };
    Ok([
        webserver.serve("/api/files/list/*", ListRoute(filesystem.clone()))?,
        webserver.serve("/api/files/raw/*", RawRoute(filesystem.clone()))?,
        webserver.serve_upload(UPLOAD, UploadEndpoint(filesystem.clone()))?,
        webserver.serve_http("/api/files/mkdir", change(Change::MakeDirectory))?,
        webserver.serve_http("/api/files/rename", change(Change::Rename))?,
        webserver.serve_http("/api/files/delete", change(Change::Delete))?,
    ])
}

/// `GET /api/files/list/<path>`: `{"entries":[{"name","type","size"}],"truncated"}`.
struct ListRoute(ScopedVfs);

impl HttpProvider for ListRoute {
    async fn serve(&self, path: &str) -> HttpResponse {
        let Some(path) = path.strip_prefix(LIST).and_then(path::decode) else {
            return refused(400, "invalid_path");
        };
        match list(&self.0, &path).await {
            Ok(listing) => json(200, &listing),
            Err(error) => failure(error),
        }
    }
}

async fn list(filesystem: &ScopedVfs, path: &str) -> Result<Listing, FsError> {
    let mut entries = filesystem
        .read_dir(path)
        .await?
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by(|left, right| {
        right
            .metadata()
            .is_dir()
            .cmp(&left.metadata().is_dir())
            .then_with(|| left.file_name().cmp(right.file_name()))
    });
    let truncated = entries.len() > MAX_ENTRIES;
    Ok(Listing {
        entries: entries
            .into_iter()
            .take(MAX_ENTRIES)
            .map(|entry| Entry {
                name: String::from(entry.file_name()),
                kind: if entry.metadata().is_dir() {
                    Kind::Dir
                } else {
                    Kind::File
                },
                size: entry.metadata().len(),
            })
            .collect(),
        truncated,
    })
}

#[derive(Serialize)]
struct Listing {
    entries: Vec<Entry>,
    truncated: bool,
}

#[derive(Serialize)]
struct Entry {
    name: String,
    #[serde(rename = "type")]
    kind: Kind,
    size: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Dir,
    File,
}

/// `GET /api/files/raw/<path>`: the file's bytes, streamed.
struct RawRoute(ScopedVfs);

impl HttpProvider for RawRoute {
    async fn serve(&self, path: &str) -> HttpResponse {
        let Some(path) = path.strip_prefix(RAW).and_then(path::decode) else {
            return refused(400, "invalid_path");
        };
        let opened = async {
            let metadata = self.0.metadata(&path).await?;
            if !metadata.is_file() {
                return Err(FsError::IsDirectory);
            }
            let length = usize::try_from(metadata.len()).map_err(|_| FsError::Unsupported)?;
            Ok((length, self.0.open(&path).await?))
        }
        .await;
        match opened {
            Ok((length, file)) => HttpResponse::stream(200, content_type(&path), length, file),
            Err(error) => failure(error),
        }
    }
}

/// Files are served under the portal's origin, so nothing is served as a type
/// the browser would run: text is plain text, and markup, SVG and scripts are
/// bytes.
fn content_type(path: &str) -> &'static str {
    let extension = path::split(path)
        .1
        .rsplit_once('.')
        .map(|(_, extension)| extension);
    match extension {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("wav") => "audio/wav",
        Some("mp3") => "audio/mpeg",
        Some(
            "txt" | "md" | "log" | "csv" | "json" | "jsonl" | "toml" | "yaml" | "yml" | "ini"
            | "conf" | "rs" | "py" | "lua" | "c" | "h",
        ) => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// `PUT /api/files/upload/<path>`: writes a new file from the request body.
struct UploadEndpoint(ScopedVfs);

impl HttpUploadEndpoint for UploadEndpoint {
    fn handle<'a>(&'a self, mut upload: HttpUpload<'a>) -> HttpFuture<'a> {
        Box::pin(async move {
            if upload.method() != HttpMethod::Put {
                return refused(405, "method_not_allowed");
            }
            let Some(path) = upload.path().strip_prefix(UPLOAD).and_then(path::decode) else {
                return refused(400, "invalid_path");
            };
            match receive(&self.0, &path, &mut upload).await {
                Ok(size) => json(
                    201,
                    &Entry {
                        name: String::from(path::split(&path).1),
                        kind: Kind::File,
                        size,
                    },
                ),
                Err(refusal) => refusal.into(),
            }
        })
    }
}

/// Streams the body to a hidden `.<name>.part` beside the target and renames
/// it into place once complete, so a broken upload never leaves a short file
/// under the real name.
async fn receive(
    filesystem: &ScopedVfs,
    path: &str,
    body: &mut HttpUpload<'_>,
) -> Result<u64, Refusal> {
    let (parent, name) = path::split(path);
    if !filesystem.metadata(parent).await?.is_dir() {
        return Err(FsError::NotDirectory.into());
    }
    absent(filesystem, path).await?;
    let partial = format!("{parent}/.{name}.part");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    let copied = match filesystem.open_with(&partial, &options).await {
        Ok(mut file) => copy(body, &mut file).await,
        Err(error) => return Err(error.into()),
    };
    let result = match copied {
        Ok(()) => filesystem
            .rename(&partial, path)
            .await
            .map_err(Refusal::from),
        Err(refusal) => Err(refusal),
    };
    if result.is_err()
        && let Err(error) = filesystem.remove_file(&partial).await
    {
        log::warn!("could not remove partial upload {partial}: {error}");
    }
    result.map(|()| body.content_length() as u64)
}

async fn copy(body: &mut HttpUpload<'_>, file: &mut File) -> Result<(), Refusal> {
    let mut buffer = [0; COPY_CHUNK];
    let mut remaining = body.content_length();
    while remaining > 0 {
        let count = body
            .read(&mut buffer[..remaining.min(COPY_CHUNK)])
            .await
            .map_err(|_| Refusal::Incomplete)?;
        if count == 0 {
            return Err(Refusal::Incomplete);
        }
        file.write_all(&buffer[..count]).await?;
        remaining -= count;
    }
    Ok(file.flush().await?)
}

/// `POST /api/files/{mkdir,rename,delete}` with a small JSON body.
struct ChangeEndpoint {
    filesystem: ScopedVfs,
    change: Change,
}

#[derive(Clone, Copy)]
enum Change {
    /// `{"path"}`: creates one directory in an existing one.
    MakeDirectory,
    /// `{"from","to"}`: renames within one directory.
    Rename,
    /// `{"path"}`: removes a file or an empty directory.
    Delete,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Move {
    from: String,
    to: String,
}

impl HttpEndpoint for ChangeEndpoint {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            if request.method() != HttpMethod::Post {
                return refused(405, "method_not_allowed");
            }
            let filesystem = &self.filesystem;
            let result = match self.change {
                Change::MakeDirectory => match target(request.body()) {
                    Some(path) => make_directory(filesystem, &path).await.map(|()| 201),
                    None => Err(Refusal::Invalid),
                },
                Change::Rename => match serde_json::from_slice::<Move>(request.body()) {
                    Ok(change) => rename(filesystem, &change.from, &change.to)
                        .await
                        .map(|()| 204),
                    Err(_) => Err(Refusal::Invalid),
                },
                Change::Delete => match target(request.body()) {
                    Some(path) => delete(filesystem, &path).await.map(|()| 204),
                    None => Err(Refusal::Invalid),
                },
            };
            match result {
                Ok(status) => HttpResponse::new(status, JSON, Vec::new()),
                Err(refusal) => refusal.into(),
            }
        })
    }
}

fn target(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Target>(body)
        .ok()
        .map(|target| target.path)
        .filter(|path| path::accepted(path))
}

async fn make_directory(filesystem: &ScopedVfs, path: &str) -> Result<(), Refusal> {
    if !filesystem.metadata(path::split(path).0).await?.is_dir() {
        return Err(FsError::NotDirectory.into());
    }
    absent(filesystem, path).await?;
    Ok(filesystem.create_dir_all(path).await?)
}

async fn rename(filesystem: &ScopedVfs, from: &str, to: &str) -> Result<(), Refusal> {
    if !path::accepted(from) || !path::accepted(to) || path::split(from).0 != path::split(to).0 {
        return Err(Refusal::Invalid);
    }
    filesystem.metadata(from).await?;
    if from != to {
        absent(filesystem, to).await?;
        filesystem.rename(from, to).await?;
    }
    Ok(())
}

async fn delete(filesystem: &ScopedVfs, path: &str) -> Result<(), Refusal> {
    if filesystem.metadata(path).await?.is_dir() {
        Ok(filesystem.remove_dir(path).await?)
    } else {
        Ok(filesystem.remove_file(path).await?)
    }
}

/// Fails with `AlreadyExists` unless nothing is at `path`.
async fn absent(filesystem: &ScopedVfs, path: &str) -> Result<(), FsError> {
    match filesystem.metadata(path).await {
        Ok(_) => Err(FsError::AlreadyExists),
        Err(FsError::NotFound) => Ok(()),
        Err(error) => Err(error),
    }
}

/// Why a change was refused before or inside the file layer.
enum Refusal {
    /// The body or a path in it was not accepted.
    Invalid,
    /// The upload ended before its declared length.
    Incomplete,
    Fs(FsError),
}

impl From<FsError> for Refusal {
    fn from(error: FsError) -> Self {
        Self::Fs(error)
    }
}

impl From<Refusal> for HttpResponse {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Invalid => refused(400, "invalid_request"),
            Refusal::Incomplete => refused(400, "incomplete_body"),
            Refusal::Fs(error) => failure(error),
        }
    }
}

fn failure(error: FsError) -> HttpResponse {
    let (status, code) = match error {
        FsError::NotFound | FsError::NotMounted => (404, "not_found"),
        FsError::AlreadyExists => (409, "exists"),
        FsError::ReadOnly | FsError::PermissionDenied => (403, "read_only"),
        FsError::DirectoryNotEmpty => (409, "not_empty"),
        FsError::IsDirectory | FsError::NotDirectory => (409, "wrong_type"),
        FsError::Busy => (409, "busy"),
        FsError::MediaRemoved => (503, "media_removed"),
        FsError::InvalidPath | FsError::InvalidInput | FsError::CrossMount => (400, "invalid_path"),
        FsError::Unsupported => (501, "unsupported"),
        _ => {
            log::warn!("file browser request failed: {error}");
            (500, "io")
        }
    };
    refused(status, code)
}

fn refused(status: u16, code: &str) -> HttpResponse {
    HttpResponse::new(
        status,
        JSON,
        format!(r#"{{"error":"{code}"}}"#).into_bytes(),
    )
}

fn json<T: Serialize>(status: u16, value: &T) -> HttpResponse {
    match serde_json::to_vec(value) {
        Ok(body) => HttpResponse::new(status, JSON, body),
        Err(error) => {
            log::error!("failed to encode file browser response: {error}");
            refused(500, "encoding")
        }
    }
}
