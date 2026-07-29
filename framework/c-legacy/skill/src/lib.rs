//! ESP-IDF adapter over the already initialized C `claw_skill` singleton.
//!
//! This crate never initializes the C registry and never registers roots. The
//! application must finish both operations before the Rust agent runtime starts.

#![cfg(target_os = "espidf")]

use core::ffi::{c_char, c_int};
use std::ffi::CString;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use claw_skill::{
    CatalogSnapshot, Skill, SkillError, SkillFrontmatterMetadata, SkillId, SkillManageMode,
    SkillRegistry, SkillRegistryVersion,
};
use serde::Deserialize;

const ESP_OK: c_int = 0;
const ESP_FAIL: c_int = -1;
const ESP_ERR_INVALID_ARG: c_int = 0x102;
const ESP_ERR_INVALID_SIZE: c_int = 0x104;
const ESP_ERR_NOT_FOUND: c_int = 0x105;

const INITIAL_BUFFER_BYTES: usize = 4 * 1024;
const MAX_CATALOG_BYTES: usize = 64 * 1024;
const MAX_DOCUMENT_BYTES: usize = 64 * 1024;

static C_SKILL_LOCK: Mutex<()> = Mutex::new(());

extern "C" {
    fn claw_skill_reload_registry() -> c_int;
    fn claw_skill_render_catalog_json(buffer: *mut c_char, size: usize) -> c_int;
    fn claw_skill_read_document(skill_id: *const c_char, buffer: *mut c_char, size: usize)
        -> c_int;
}

/// Rust view over the process-global C skill registry.
pub struct LegacySkillRegistry {
    snapshot: RwLock<Arc<CatalogSnapshot>>,
    next_version: AtomicU32,
}

impl LegacySkillRegistry {
    /// Attach to C state that the application has already initialized.
    ///
    /// This performs one read-only catalog import. It does not initialize,
    /// reload, or add directories to the C registry.
    pub fn attach() -> Result<Self, SkillError> {
        let _guard = lock_c_skill();
        let snapshot = read_catalog_unlocked(1)?;
        Ok(Self {
            snapshot: RwLock::new(Arc::new(snapshot)),
            next_version: AtomicU32::new(2),
        })
    }
}

impl SkillRegistry for LegacySkillRegistry {
    fn catalog(&self) -> Arc<CatalogSnapshot> {
        Arc::clone(&self.snapshot.read().unwrap_or_else(PoisonError::into_inner))
    }

    fn reload(&self) -> Result<(), SkillError> {
        let _guard = lock_c_skill();
        // SAFETY: the C application owns a live initialized singleton for the
        // entire Rust runtime lifetime; the process-wide mutex serializes calls.
        let result = unsafe { claw_skill_reload_registry() };
        check_backend_result("reload_registry", result)?;

        let version = self.next_version.fetch_add(1, Ordering::Relaxed);
        let snapshot = Arc::new(read_catalog_unlocked(version)?);
        *self
            .snapshot
            .write()
            .unwrap_or_else(PoisonError::into_inner) = snapshot;
        Ok(())
    }

    fn load_document_into(&self, id: &SkillId, out: &mut String) -> Result<(), SkillError> {
        let skill_id = CString::new(id.as_str()).map_err(|_| SkillError::Backend {
            operation: "read_document",
            code: ESP_ERR_INVALID_ARG,
        })?;
        let _guard = lock_c_skill();
        let document = read_c_buffer("read_document", MAX_DOCUMENT_BYTES, |buffer, size| {
            // SAFETY: `skill_id` is NUL terminated and both pointers remain
            // valid for the duration of this synchronous C call.
            unsafe { claw_skill_read_document(skill_id.as_ptr(), buffer, size) }
        })
        .map_err(|error| match error {
            SkillError::Backend {
                operation: _,
                code: ESP_ERR_NOT_FOUND,
            } => SkillError::NotFound(id.clone()),
            other => other,
        })?;

        append_wrapped_document(id, &document, out);
        Ok(())
    }
}

#[derive(Deserialize)]
struct LegacyCatalog {
    skills: Vec<LegacyCatalogEntry>,
}

#[derive(Deserialize)]
struct LegacyCatalogEntry {
    id: String,
    file: String,
    summary: String,
    cap_groups: Vec<String>,
    manage_mode: String,
}

fn read_catalog_unlocked(version: SkillRegistryVersion) -> Result<CatalogSnapshot, SkillError> {
    let rendered = read_c_buffer("render_catalog_json", MAX_CATALOG_BYTES, |buffer, size| {
        // SAFETY: `buffer` is writable for `size` bytes during this
        // synchronous C call.
        unsafe { claw_skill_render_catalog_json(buffer, size) }
    })?;
    let catalog: LegacyCatalog =
        serde_json::from_str(&rendered).map_err(|_| SkillError::Backend {
            operation: "parse_catalog_json",
            code: ESP_FAIL,
        })?;
    let mut skills = Vec::with_capacity(catalog.skills.len());
    for entry in catalog.skills {
        let manage_mode = SkillManageMode::try_from(entry.manage_mode.as_str()).map_err(|_| {
            SkillError::Backend {
                operation: "parse_catalog_manage_mode",
                code: ESP_FAIL,
            }
        })?;
        let metadata = SkillFrontmatterMetadata::new(
            entry.cap_groups,
            manage_mode,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        skills.push(Skill::from_catalog_entry(
            SkillId::new(entry.id),
            entry.summary,
            entry.file,
            metadata,
        ));
    }
    skills.sort_by(|left, right| left.id().cmp(right.id()));
    Ok(CatalogSnapshot::from_skills(version, skills))
}

fn read_c_buffer(
    operation: &'static str,
    max_capacity: usize,
    call: impl Fn(*mut c_char, usize) -> c_int,
) -> Result<String, SkillError> {
    let mut capacity = INITIAL_BUFFER_BYTES.min(max_capacity);
    loop {
        let mut buffer = vec![0_u8; capacity];
        let result = call(buffer.as_mut_ptr().cast(), buffer.len());
        if result == ESP_OK {
            let end = buffer
                .iter()
                .position(|byte| *byte == 0)
                .ok_or(SkillError::Backend {
                    operation,
                    code: ESP_ERR_INVALID_SIZE,
                })?;
            let content = buffer.get(..end).ok_or(SkillError::Backend {
                operation,
                code: ESP_ERR_INVALID_SIZE,
            })?;
            return String::from_utf8(content.to_vec()).map_err(|_| SkillError::Backend {
                operation,
                code: ESP_FAIL,
            });
        }
        if result != ESP_ERR_INVALID_SIZE || capacity >= max_capacity {
            return Err(SkillError::Backend {
                operation,
                code: result,
            });
        }
        capacity = capacity
            .checked_mul(2)
            .unwrap_or(max_capacity)
            .min(max_capacity);
    }
}

fn check_backend_result(operation: &'static str, result: c_int) -> Result<(), SkillError> {
    if result == ESP_OK {
        Ok(())
    } else {
        Err(SkillError::Backend {
            operation,
            code: result,
        })
    }
}

fn append_wrapped_document(id: &SkillId, document: &str, out: &mut String) {
    out.push_str("<skill_content name=\"");
    append_xml_attribute_escaped(id.as_str(), out);
    out.push_str("\">\n");
    out.push_str(document);
    out.push_str("\n</skill_content>");
}

fn append_xml_attribute_escaped(text: &str, out: &mut String) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
}

fn lock_c_skill() -> std::sync::MutexGuard<'static, ()> {
    C_SKILL_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}
