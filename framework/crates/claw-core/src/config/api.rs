//! Multi-key LLM API configuration management.
//!
//! A system may hold several LLM API configs (different providers, keys, or
//! models) used for different purposes. [`ClawApiManager`] registers configs
//! against an [`ApiPurpose`] and resolves the right one per purpose, falling
//! back to a registered default.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use claw_api::{ClawApiConfig, InitError};

/// What an LLM API config is used for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ApiPurpose {
    /// The root (externally-visible) agent's turns.
    RootAgent,
    /// Spawned subagents' turns.
    SubAgent,
    /// Long-term memory extraction / recall calls.
    Memory,
    /// Session-history compaction calls.
    Compaction,
}

pub(crate) type SharedApiManager = Arc<RwLock<ClawApiManager>>;

/// Registers LLM API configs per [`ApiPurpose`], de-duplicated by model, with a
/// default fallback.
///
/// Configs are keyed by their `model`: [`link_api`](Self::link_api) with a model
/// that already exists **replaces** the stored config (e.g. to rotate a key), and
/// every purpose bound to that model then resolves to the updated config.
#[derive(Debug, Default)]
pub(crate) struct ClawApiManager {
    /// Configs by model name (one per model).
    by_model: HashMap<String, ClawApiConfig>,
    /// Purpose → the model name it resolves to.
    by_purpose: HashMap<ApiPurpose, String>,
    /// Model resolved for a purpose that has no explicit binding.
    default_model: Option<String>,
}

impl ClawApiManager {
    /// Register `api` for `purpose`.
    ///
    /// If a config with the same `model` is already stored it is replaced, so
    /// every purpose bound to that model sees the new config. When `default` is
    /// `true`, this model becomes the fallback for purposes without an explicit
    /// binding (the most recent `default` link wins).
    ///
    /// # Errors
    ///
    /// Returns [`InitError`] without changing the manager when `api` is invalid.
    pub(crate) fn link_api(
        &mut self,
        api: ClawApiConfig,
        purpose: ApiPurpose,
        default: bool,
    ) -> Result<(), InitError> {
        api.validate()?;
        let model = api.model.clone();
        self.by_model.insert(model.clone(), api);
        self.by_purpose.insert(purpose, model.clone());
        if default {
            self.default_model = Some(model);
        }
        Ok(())
    }

    /// Resolve the config for `purpose`: its explicit binding if present,
    /// otherwise the default, otherwise `None`.
    #[must_use]
    pub(crate) fn get_api(&self, purpose: ApiPurpose) -> Option<ClawApiConfig> {
        let model = self
            .by_purpose
            .get(&purpose)
            .or(self.default_model.as_ref())?;
        self.by_model.get(model).cloned()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use claw_api::BackendKind;

    fn cfg(model: &str, key: &str) -> ClawApiConfig {
        ClawApiConfig::new(
            BackendKind::OpenAiCompatible,
            key,
            model,
            "http://api.test/v1",
        )
    }

    #[test]
    fn empty_manager_resolves_nothing() {
        let manager = ClawApiManager::default();
        assert_eq!(manager.get_api(ApiPurpose::RootAgent), None);
    }

    #[test]
    fn explicit_binding_takes_precedence_over_default() {
        let mut manager = ClawApiManager::default();
        manager
            .link_api(cfg("default-model", "k0"), ApiPurpose::Memory, true)
            .unwrap();
        manager
            .link_api(cfg("root-model", "k1"), ApiPurpose::RootAgent, false)
            .unwrap();

        assert_eq!(
            manager.get_api(ApiPurpose::RootAgent).unwrap().model,
            "root-model"
        );
        // Memory has its own binding; both it and unbound purposes differ correctly.
        assert_eq!(
            manager.get_api(ApiPurpose::Memory).unwrap().model,
            "default-model"
        );
    }

    #[test]
    fn unbound_purpose_falls_back_to_default() {
        let mut manager = ClawApiManager::default();
        manager
            .link_api(cfg("default-model", "k0"), ApiPurpose::RootAgent, true)
            .unwrap();
        // SubAgent was never linked -> falls back to the default.
        assert_eq!(
            manager.get_api(ApiPurpose::SubAgent).unwrap().model,
            "default-model"
        );
    }

    #[test]
    fn unbound_purpose_without_default_is_none() {
        let mut manager = ClawApiManager::default();
        manager
            .link_api(cfg("root-model", "k1"), ApiPurpose::RootAgent, false)
            .unwrap();
        assert_eq!(manager.get_api(ApiPurpose::Compaction), None);
    }

    #[test]
    fn invalid_config_is_rejected_without_mutating_bindings() {
        let mut manager = ClawApiManager::default();
        let invalid = cfg("invalid", "");

        assert_eq!(
            manager.link_api(invalid, ApiPurpose::RootAgent, true),
            Err(InitError::MissingApiKey)
        );
        assert_eq!(manager.get_api(ApiPurpose::RootAgent), None);
    }

    #[test]
    fn linking_same_model_replaces_and_updates_all_bindings() {
        let mut manager = ClawApiManager::default();
        manager
            .link_api(cfg("shared", "old-key"), ApiPurpose::RootAgent, false)
            .unwrap();
        manager
            .link_api(cfg("shared", "old-key"), ApiPurpose::Memory, false)
            .unwrap();
        // Re-link the same model with a rotated key.
        manager
            .link_api(cfg("shared", "new-key"), ApiPurpose::RootAgent, false)
            .unwrap();

        assert_eq!(
            manager.get_api(ApiPurpose::RootAgent).unwrap().api_key,
            "new-key"
        );
        // The other purpose bound to the same model sees the rotated key too.
        assert_eq!(
            manager.get_api(ApiPurpose::Memory).unwrap().api_key,
            "new-key"
        );
    }
}
