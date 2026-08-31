//! Filesystem-backed skill registry.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::sync::atomic::{AtomicU32, Ordering};

use barracuda_vfs::{FsError, ScopedVfs};
use getset::CopyGetters;

use super::document::{frontmatter_sections, parse_frontmatter, Skill, SkillError, SkillName};
use super::skill_set::SkillSet;

pub type SkillRegistryVersion = u32;
pub(crate) type SkillFuture<'a, T> =
    core::pin::Pin<alloc::boxed::Box<dyn core::future::Future<Output = T> + 'a>>;

/// Immutable point-in-time catalog view.
#[derive(CopyGetters, Debug)]
pub struct CatalogSnapshot {
    /// Snapshot version, bumped by every successful registry reload.
    #[getset(get_copy = "pub")]
    version: SkillRegistryVersion,
    skills: Arc<[Skill]>,
}

impl CatalogSnapshot {
    pub(crate) fn empty() -> Self {
        Self {
            version: 0,
            skills: Arc::from([]),
        }
    }

    /// Build an owned catalog snapshot supplied by an external registry.
    pub fn from_skills(version: SkillRegistryVersion, skills: Vec<Skill>) -> Self {
        Self {
            version,
            skills: Arc::from(skills),
        }
    }

    /// Skills sorted by id, with root priority already resolved.
    pub fn skills(&self) -> &[Skill] {
        &self.skills
    }

    /// Look up one skill by id.
    pub fn get(&self, name: &SkillName) -> Option<&Skill> {
        self.skills.iter().find(|skill| skill.name() == name)
    }
}

/// Shared skill catalog and document backend.
///
/// Implementations own discovery and document loading. [`SkillSet`] adds the
/// per-agent render buffers over this shared registry.
pub trait SkillRegistry: 'static {
    /// Create a per-agent [`SkillSet`] projection backed by this registry.
    fn skill_set(self: Arc<Self>) -> SkillSet {
        let registry: Arc<dyn SkillRegistry> = Arc::new(ErasedSkillRegistry(self));
        SkillSet::from_registry(registry)
    }

    /// Return the current immutable catalog snapshot.
    fn catalog(&self) -> Arc<CatalogSnapshot>;

    /// Refresh the registry while preserving the previous snapshot on failure.
    fn reload(&self) -> SkillFuture<'_, Result<(), SkillError>>;

    /// Read one skill's Markdown instructions into `out`.
    fn read_document<'a>(
        &'a self,
        name: &'a SkillName,
    ) -> SkillFuture<'a, Result<String, SkillError>>;
}

struct ErasedSkillRegistry<R: SkillRegistry + ?Sized>(Arc<R>);

impl<R: SkillRegistry + ?Sized> SkillRegistry for ErasedSkillRegistry<R> {
    fn skill_set(self: Arc<Self>) -> SkillSet {
        SkillSet::from_registry(self)
    }

    fn catalog(&self) -> Arc<CatalogSnapshot> {
        self.0.catalog()
    }

    fn reload(&self) -> SkillFuture<'_, Result<(), SkillError>> {
        self.0.reload()
    }

    fn read_document<'a>(
        &'a self,
        name: &'a SkillName,
    ) -> SkillFuture<'a, Result<String, SkillError>> {
        self.0.read_document(name)
    }
}

/// Empty registry used by agents without skill backing.
#[derive(Debug, Default)]
pub struct EmptySkillRegistry;

impl SkillRegistry for EmptySkillRegistry {
    fn skill_set(self: Arc<Self>) -> SkillSet {
        SkillSet::from_registry(self)
    }

    fn catalog(&self) -> Arc<CatalogSnapshot> {
        Arc::new(CatalogSnapshot::empty())
    }

    fn reload(&self) -> SkillFuture<'_, Result<(), SkillError>> {
        Box::pin(async { Ok(()) })
    }

    fn read_document<'a>(
        &'a self,
        name: &'a SkillName,
    ) -> SkillFuture<'a, Result<String, SkillError>> {
        let name = name.clone();
        Box::pin(async move { Err(SkillError::NotFound(name)) })
    }
}

/// Filesystem-backed registry over one or more priority-ordered skill roots.
pub struct FsSkillRegistry {
    filesystem: ScopedVfs,
    roots: Vec<String>,
    snapshot: RefCell<Arc<CatalogSnapshot>>,
    next_version: AtomicU32,
}

impl FsSkillRegistry {
    /// Create an empty registry builder.
    pub fn new(filesystem: ScopedVfs) -> Self {
        Self {
            filesystem,
            roots: Vec::new(),
            snapshot: RefCell::new(Arc::new(CatalogSnapshot::empty())),
            next_version: AtomicU32::new(1),
        }
    }

    /// Append one skills root, rescan, and return the registry builder.
    ///
    /// Add roots in priority order, e.g. DATA before SYSTEM.
    pub async fn set_root(mut self, root: impl Into<String>) -> Result<Self, SkillError> {
        self.roots.push(root.into());
        let snapshot = self.scan_catalog_next_version().await?;
        *self.snapshot.borrow_mut() = Arc::new(snapshot);
        Ok(self)
    }

    /// Create a per-agent [`SkillSet`] projection backed by this registry.
    pub fn skill_set(self: &Arc<Self>) -> SkillSet {
        let registry: Arc<dyn SkillRegistry> = self.clone();
        SkillSet::from_registry(registry)
    }

    pub(crate) fn catalog(&self) -> Arc<CatalogSnapshot> {
        Arc::clone(&self.snapshot.borrow())
    }

    pub(crate) async fn reload(&self) -> Result<(), SkillError> {
        let snapshot = self.scan_catalog_next_version().await?;
        *self.snapshot.borrow_mut() = Arc::new(snapshot);
        Ok(())
    }

    pub(crate) async fn read_document(&self, name: &SkillName) -> Result<String, SkillError> {
        let snapshot = self.catalog();
        let skill = snapshot
            .get(name)
            .ok_or_else(|| SkillError::NotFound(name.clone()))?;
        let directory = skill.directory().ok_or(SkillError::Backend {
            operation: "read_document",
            code: -1,
        })?;
        let path = format!("{directory}/SKILL.md");
        let bytes = self
            .read_skill_document(&path)
            .await
            .map_err(|error| SkillError::ReadFailed(name.clone(), error))?;
        let text = String::from_utf8(bytes).map_err(|_| SkillError::InvalidUtf8(name.clone()))?;
        let (_, body) = frontmatter_sections(name, &text)?;
        Ok(body.trim().into())
    }

    async fn scan_catalog_next_version(&self) -> Result<CatalogSnapshot, SkillError> {
        let version = self.next_version.fetch_add(1, Ordering::Relaxed);
        scan_catalog(&self.filesystem, &self.roots, version).await
    }

    async fn read_skill_document(&self, path: &str) -> Result<Vec<u8>, FsError> {
        self.filesystem.read(path).await
    }
}

impl SkillRegistry for FsSkillRegistry {
    fn skill_set(self: Arc<Self>) -> SkillSet {
        SkillSet::from_registry(self)
    }

    fn catalog(&self) -> Arc<CatalogSnapshot> {
        FsSkillRegistry::catalog(self)
    }

    fn reload(&self) -> SkillFuture<'_, Result<(), SkillError>> {
        Box::pin(FsSkillRegistry::reload(self))
    }

    fn read_document<'a>(
        &'a self,
        name: &'a SkillName,
    ) -> SkillFuture<'a, Result<String, SkillError>> {
        Box::pin(FsSkillRegistry::read_document(self, name))
    }
}

async fn scan_catalog(
    filesystem: &ScopedVfs,
    roots: &[String],
    version: SkillRegistryVersion,
) -> Result<CatalogSnapshot, SkillError> {
    let mut skills = Vec::new();
    for root in roots {
        let names = match filesystem.list_dir(root).await {
            Ok(names) => names,
            Err(FsError::NotFound) => continue,
            Err(error) => return Err(SkillError::ScanFailed(root.clone(), error)),
        };
        for name in names {
            let name = SkillName::new(name);
            if skills.iter().any(|skill: &Skill| skill.name() == &name) {
                continue;
            }
            let path = skill_document_path(root, name.as_str());
            if !filesystem
                .exists(&path)
                .await
                .map_err(|error| SkillError::ScanFailed(root.clone(), error))?
            {
                continue;
            }
            let document = read_document(filesystem, &name, &path).await?;
            skills.push(parse_frontmatter(name, root, &document)?);
        }
    }
    skills.sort_by(|left, right| left.name().cmp(right.name()));
    Ok(CatalogSnapshot {
        version,
        skills: Arc::from(skills),
    })
}

fn skill_document_path(root: &str, id: &str) -> String {
    format!("{}/SKILL.md", skill_directory_path(root, id))
}

fn skill_directory_path(root: &str, id: &str) -> String {
    format!("{}/{}", root.trim_end_matches('/'), id)
}

async fn read_document(
    filesystem: &ScopedVfs,
    name: &SkillName,
    path: &str,
) -> Result<String, SkillError> {
    let bytes = filesystem
        .read(path)
        .await
        .map_err(|error| SkillError::ReadFailed(name.clone(), error))?;
    String::from_utf8(bytes).map_err(|_| SkillError::InvalidUtf8(name.clone()))
}
