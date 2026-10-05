//! Per-agent skill projection and reusable render buffers.

use alloc::boxed::Box;
use alloc::string::String;
use core::fmt::Write as _;
use portable_atomic_util::Arc;

use super::document::{SkillDocument, SkillError, SkillName, SkillResourcePage};
use super::registry::{CatalogSnapshot, EmptySkillRegistry, SkillRegistry, SkillRegistryVersion};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CatalogBufferKind {
    Empty,
    ListJson,
    Context,
}

/// Per-agent skill view and cache.
#[derive(Clone)]
pub struct SkillSet {
    registry: Arc<dyn SkillRegistry>,
    catalog_version: SkillRegistryVersion,
    catalog_buffer_kind: CatalogBufferKind,
    catalog_buffer: String,
    document_buffer: String,
}

impl SkillSet {
    /// A skill set over an empty registry.
    pub fn empty() -> Self {
        Self::from_registry(Arc::from(
            Box::new(EmptySkillRegistry) as Box<dyn SkillRegistry>
        ))
    }

    pub(crate) fn from_registry(registry: Arc<dyn SkillRegistry>) -> Self {
        Self {
            registry,
            catalog_version: 0,
            catalog_buffer_kind: CatalogBufferKind::Empty,
            catalog_buffer: String::new(),
            document_buffer: String::new(),
        }
    }

    /// Re-scan the backing registry. The next catalog render observes the new
    /// snapshot version and refreshes its cache.
    pub async fn reload(&self) -> Result<(), SkillError> {
        self.registry.reload().await
    }

    /// JSON catalog for tool output. The returned borrow is valid until the next
    /// mutable method call on this `SkillSet`.
    pub fn list_skills(&mut self) -> &str {
        let snapshot = self.registry.catalog();
        if !self.catalog_cache_is_fresh(&snapshot, CatalogBufferKind::ListJson) {
            self.render_list_json(&snapshot);
        }
        &self.catalog_buffer
    }

    /// Prompt-facing catalog summary. The returned borrow is valid until the
    /// next mutable method call on this `SkillSet`.
    pub fn catalog_context(&mut self) -> &str {
        let snapshot = self.registry.catalog();
        if !self.catalog_cache_is_fresh(&snapshot, CatalogBufferKind::Context) {
            self.render_catalog_context(&snapshot);
        }
        &self.catalog_buffer
    }

    /// Read one skill's Markdown instructions on demand.
    pub async fn read_skill(&mut self, name: &SkillName) -> Result<SkillDocument, SkillError> {
        self.document_buffer.clear();
        self.document_buffer = self.registry.read_document(name).await?;
        Ok(SkillDocument::new(self.document_buffer.clone()))
    }

    /// Read one bounded UTF-8 page from a relative file inside a skill.
    pub async fn read_resource(
        &self,
        name: &SkillName,
        path: &str,
        offset: u64,
        limit: usize,
    ) -> Result<SkillResourcePage, SkillError> {
        self.registry.read_resource(name, path, offset, limit).await
    }

    fn catalog_cache_is_fresh(&self, snapshot: &CatalogSnapshot, kind: CatalogBufferKind) -> bool {
        self.catalog_version == snapshot.version() && self.catalog_buffer_kind == kind
    }

    fn render_list_json(&mut self, snapshot: &CatalogSnapshot) {
        self.catalog_buffer.clear();
        self.catalog_buffer.push('[');
        for (index, skill) in snapshot.skills().iter().enumerate() {
            if index > 0 {
                self.catalog_buffer.push(',');
            }
            self.catalog_buffer.push('{');
            push_json_field(
                &mut self.catalog_buffer,
                "name",
                skill.name().as_str(),
                false,
            );
            push_json_field(
                &mut self.catalog_buffer,
                "description",
                skill.description(),
                true,
            );
            self.catalog_buffer.push('}');
        }
        self.catalog_buffer.push(']');
        self.catalog_version = snapshot.version();
        self.catalog_buffer_kind = CatalogBufferKind::ListJson;
    }

    fn render_catalog_context(&mut self, snapshot: &CatalogSnapshot) {
        self.catalog_buffer.clear();
        self.catalog_buffer.push_str("Available skills:\n");
        for skill in snapshot.skills() {
            self.catalog_buffer.push_str("- ");
            self.catalog_buffer.push_str(skill.name().as_str());
            self.catalog_buffer.push_str(": ");
            self.catalog_buffer.push_str(skill.description());
            self.catalog_buffer.push('\n');
        }
        self.catalog_version = snapshot.version();
        self.catalog_buffer_kind = CatalogBufferKind::Context;
    }
}

fn push_json_field(out: &mut String, key: &str, value: &str, comma: bool) {
    if comma {
        out.push(',');
    }
    push_json_string(out, key);
    out.push(':');
    push_json_string(out, value);
}

fn push_json_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => {
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}
