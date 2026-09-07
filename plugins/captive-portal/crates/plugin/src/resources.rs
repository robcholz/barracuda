use crate::portal::{not_found, valid_path};
use crate::AssetsProvider;
use alloc::format;
use alloc::vec::Vec;
use barracuda_vfs::{FileType, FsError, ScopedVfs};
use barracuda_webserver_plugin::HttpResponse;

/// Serves files exclusively beneath the supplied plugin scope's `/resources`.
/// This adapter never mounts storage or accesses another plugin's namespace.
pub struct ResourceFiles(ScopedVfs);

impl From<ScopedVfs> for ResourceFiles {
    fn from(filesystem: ScopedVfs) -> Self {
        Self(filesystem)
    }
}

impl AssetsProvider for ResourceFiles {
    async fn serve(&self, path: &str) -> HttpResponse {
        if !valid_path(path) {
            return not_found();
        }
        let resource = format!("/resources/{path}");
        let result = async {
            let metadata = self.0.metadata(&resource).await?;
            if metadata.file_type() != FileType::File {
                return Ok(None);
            }
            let file = self.0.open(&resource).await?;
            Ok::<_, FsError>(Some((metadata.len(), file)))
        }
        .await;
        match result {
            Ok(Some((length, file))) => match usize::try_from(length) {
                Ok(length) => HttpResponse::stream(200, content_type(path), length, file),
                Err(_) => HttpResponse::new(500, "text/plain", Vec::new()),
            },
            Ok(None) | Err(FsError::NotFound) => not_found(),
            Err(FsError::NotMounted) => HttpResponse::new(503, "text/plain", Vec::new()),
            Err(_) => HttpResponse::new(500, "text/plain", Vec::new()),
        }
    }
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "application/javascript",
        Some("css") => "text/css",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}
