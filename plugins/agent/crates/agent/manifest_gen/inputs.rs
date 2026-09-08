//! Resource inputs watched by the Agent manifest generator.

/// The manifest files expected in every kind directory; also the set the build
/// script registers for `rerun-if-changed`.
pub(crate) const MANIFEST_FILES: &[&str] = &["agent.json", "tools/tools.json", "instructions.md"];

/// Files the shared `common/` base is tracked for `rerun-if-changed`.
pub(crate) const COMMON_FILES: &[&str] = &["tools/tools.json", "instructions.md"];
