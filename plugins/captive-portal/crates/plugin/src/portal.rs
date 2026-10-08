use alloc::format;
use alloc::rc::{Rc, Weak};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use crate::AssetsProvider;
use barracuda_webserver_plugin::{HttpProviderHandle, HttpResponse};
use embedded_io_async::{ErrorType, Read};
use serde::Serialize;

/// Where an entry sits in the portal's navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WebGroup {
    /// Device settings, such as networking.
    Device,
    /// Agent and model configuration.
    Agent,
    /// Message channels.
    Channel,
}

/// A label in both portal languages (JSON escaped by the portal).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct WebText {
    /// Simplified Chinese text.
    pub zh: &'static str,
    /// English text.
    pub en: &'static str,
}

/// One independently removable navigation entry.
#[derive(Clone, Copy, Debug)]
pub struct WebEntry {
    /// Stable plugin ID, used as the resource namespace.
    pub id: &'static str,
    /// Navigation group containing this entry.
    pub group: WebGroup,
    /// Position within the group, ascending. The manifest keeps registration
    /// order; the portal page sorts by group, order, then ID.
    pub order: u8,
    /// Navigation label.
    pub title: WebText,
    /// One line describing the entry on overview tiles and rows.
    pub summary: WebText,
    /// Optional relative icon path within this provider, for example
    /// `icon.svg`; `.svg` icons are monochrome, other formats are images.
    pub icon: Option<&'static str>,
    /// Optional relative module path of a live figure within this provider,
    /// for example `figure.js`.
    pub figure: Option<&'static str>,
    /// Relative entry module within this provider, for example `entry.js`.
    pub module: &'static str,
}

/// Failure registering a web entry.
#[derive(Debug, thiserror::Error)]
pub enum PortalError {
    /// The ID, module, icon, or figure path is invalid.
    #[error("invalid portal entry ID or asset path")]
    InvalidEntry,
    /// Another entry owns this ID.
    #[error("portal entry ID is already registered")]
    DuplicateEntry,
    /// The entry could not be encoded for the navigation manifest.
    #[error(transparent)]
    Manifest(#[from] serde_json::Error),
}

struct Entry {
    id: &'static str,
    json: Vec<u8>,
    assets: HttpProviderHandle,
}

struct Registry {
    entries: Vec<Entry>,
    manifest: Rc<[u8]>,
}

impl Registry {
    fn rebuild(&mut self) {
        let mut bytes = Vec::new();
        bytes.push(b'[');
        for (index, entry) in self.entries.iter().enumerate() {
            if index != 0 {
                bytes.push(b',');
            }
            bytes.extend_from_slice(&entry.json);
        }
        bytes.push(b']');
        self.manifest = bytes.into();
    }
}

/// Aggregates entries and assets without registering a route for each plugin.
pub struct CaptivePortal {
    registry: Rc<RefCell<Registry>>,
    scaffold: HttpProviderHandle,
}

impl CaptivePortal {
    /// Creates an aggregator with the provider for its own scaffold files.
    pub fn new(scaffold: impl AssetsProvider) -> Self {
        Self {
            registry: Rc::new(RefCell::new(Registry {
                entries: Vec::new(),
                manifest: Rc::from(&b"[]"[..]),
            })),
            scaffold: HttpProviderHandle::new(scaffold),
        }
    }

    /// Registers navigation metadata and its asset provider as one lifetime.
    /// Retain the returned guard with the consuming plugin's register context.
    ///
    /// # Errors
    /// Rejects invalid paths, duplicate IDs, and manifest serialization errors.
    pub fn register(
        &self,
        entry: WebEntry,
        assets: impl AssetsProvider,
    ) -> Result<WebEntryRegistration, PortalError> {
        if entry.id.is_empty()
            || entry.id.len() > 64
            || !entry.id.split('-').all(|segment| {
                !segment.is_empty()
                    && segment
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            })
            || !valid_path(entry.module)
            || entry.icon.is_some_and(|path| !valid_path(path))
            || entry.figure.is_some_and(|path| !valid_path(path))
        {
            return Err(PortalError::InvalidEntry);
        }
        let mut registry = self.registry.borrow_mut();
        if registry.entries.iter().any(|old| old.id == entry.id) {
            return Err(PortalError::DuplicateEntry);
        }
        #[derive(Serialize)]
        struct ManifestEntry<'a> {
            id: &'a str,
            group: WebGroup,
            order: u8,
            title: WebText,
            summary: WebText,
            icon: Option<String>,
            figure: Option<String>,
            module: String,
        }
        let asset = |path: &str| format!("/portal/assets/{}/{path}", entry.id);
        let json = serde_json::to_vec(&ManifestEntry {
            id: entry.id,
            group: entry.group,
            order: entry.order,
            title: entry.title,
            summary: entry.summary,
            icon: entry.icon.map(asset),
            figure: entry.figure.map(asset),
            module: asset(entry.module),
        })?;
        registry.entries.push(Entry {
            id: entry.id,
            json,
            assets: HttpProviderHandle::new(assets),
        });
        registry.rebuild();
        Ok(WebEntryRegistration {
            id: entry.id,
            registry: Rc::downgrade(&self.registry),
        })
    }
}

impl AssetsProvider for CaptivePortal {
    async fn serve(&self, path: &str) -> HttpResponse {
        let Some(relative) = path.strip_prefix("/portal/") else {
            return not_found();
        };
        if relative == "entries.json" {
            let bytes = self.registry.borrow().manifest.clone();
            return HttpResponse::stream(
                200,
                "application/json",
                bytes.len(),
                ManifestReader { bytes, offset: 0 },
            );
        }
        if let Some(asset) = relative.strip_prefix("assets/") {
            let Some((id, path)) = asset.split_once('/') else {
                return not_found();
            };
            if !valid_path(path) {
                return not_found();
            }
            let provider = self
                .registry
                .borrow()
                .entries
                .iter()
                .find(|entry| entry.id == id)
                .map(|entry| entry.assets.clone());
            return match provider {
                Some(provider) => provider.serve(path).await,
                None => not_found(),
            };
        }
        let path = if relative.is_empty() {
            "index.html"
        } else {
            relative
        };
        if !valid_path(path) {
            return not_found();
        }
        self.scaffold.serve(path).await
    }
}

/// Removes both navigation metadata and asset access when dropped.
/// In-flight responses may finish using their retained source.
#[must_use = "retain the guard to keep the entry registered"]
pub struct WebEntryRegistration {
    id: &'static str,
    registry: Weak<RefCell<Registry>>,
}

impl Drop for WebEntryRegistration {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            let mut registry = registry.borrow_mut();
            registry.entries.retain(|entry| entry.id != self.id);
            registry.rebuild();
        }
    }
}

pub(crate) fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 256
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b))
}

pub(crate) fn not_found() -> HttpResponse {
    HttpResponse::new(404, "text/plain", Vec::new())
}

struct ManifestReader {
    bytes: Rc<[u8]>,
    offset: usize,
}
impl ErrorType for ManifestReader {
    type Error = core::convert::Infallible;
}
impl Read for ManifestReader {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        let count = buffer.len().min(self.bytes.len() - self.offset);
        buffer[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }
}
