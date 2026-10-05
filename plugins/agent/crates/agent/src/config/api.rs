//! Multi-key LLM API configuration management.
//!
//! A system may hold several LLM API configs (different providers, keys, or
//! models) used for different purposes. [`ModelApiManager`] registers configs
//! against an [`ApiPurpose`] and resolves the right one per purpose, falling
//! back to a registered default.

use alloc::{collections::BTreeMap, string::String};
use core::cell::RefCell;

use barracuda_model_api::{InitError, ModelApiConfig};
use portable_atomic_util::Arc;

/// What an LLM API config is used for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

pub type SharedApiManager = Arc<RefCell<ModelApiManager>>;

/// Registers LLM API configs per [`ApiPurpose`], de-duplicated by model, with a
/// default fallback.
///
/// Configs are keyed by their `model`: [`set_api`](Self::set_api) with a model
/// that already exists **replaces** the stored config (e.g. to rotate a key), and
/// every purpose bound to that model then resolves to the updated config.
#[derive(Clone, Debug, Default)]
pub struct ModelApiManager {
    /// Configs by model name (one per model).
    by_model: BTreeMap<String, ModelApiConfig>,
    /// Purpose → the model name it resolves to.
    by_purpose: BTreeMap<ApiPurpose, String>,
    /// Model resolved for a purpose that has no explicit binding.
    default_model: Option<String>,
}

impl ModelApiManager {
    /// Register `api` for `purpose`.
    ///
    /// If a config with the same `model` is already stored it is replaced, so
    /// every purpose bound to that model sees the new config. When `default` is
    /// `true`, this model becomes the fallback for purposes without an explicit
    /// binding (the most recent default assignment wins).
    ///
    /// # Errors
    ///
    /// Returns [`InitError`] without changing the manager when `api` is invalid.
    pub fn set_api(
        &mut self,
        api: ModelApiConfig,
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
    pub fn get_api(&self, purpose: ApiPurpose) -> Option<ModelApiConfig> {
        let model = self
            .by_purpose
            .get(&purpose)
            .or(self.default_model.as_ref())?;
        self.by_model.get(model).cloned()
    }

    /// Returns the API explicitly bound to `purpose`, excluding default fallback.
    #[must_use]
    pub fn get_explicit_api(&self, purpose: ApiPurpose) -> Option<ModelApiConfig> {
        let model = self.by_purpose.get(&purpose)?;
        self.by_model.get(model).cloned()
    }

    /// Returns the configured default API.
    #[must_use]
    pub fn get_default_api(&self) -> Option<ModelApiConfig> {
        let model = self.default_model.as_ref()?;
        self.by_model.get(model).cloned()
    }

    /// Installs an API as the fallback without changing any purpose binding.
    ///
    /// # Errors
    ///
    /// Returns [`InitError`] without changing the manager when `api` is invalid.
    pub fn set_default_api(&mut self, api: ModelApiConfig) -> Result<(), InitError> {
        api.validate()?;
        let model = api.model.clone();
        self.by_model.insert(model.clone(), api);
        self.default_model = Some(model);
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use barracuda_model_api::BackendKind;

    fn cfg(model: &str, key: &str) -> ModelApiConfig {
        ModelApiConfig::new(
            BackendKind::OpenAiCompatible,
            key,
            model,
            "http://api.test/v1",
        )
    }

    #[test]
    fn empty_manager_resolves_nothing() {
        let manager = ModelApiManager::default();
        assert_eq!(manager.get_api(ApiPurpose::RootAgent), None);
    }

    #[test]
    fn explicit_binding_takes_precedence_over_default() {
        let mut manager = ModelApiManager::default();
        manager
            .set_api(cfg("default-model", "k0"), ApiPurpose::Memory, true)
            .unwrap();
        manager
            .set_api(cfg("root-model", "k1"), ApiPurpose::RootAgent, false)
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
        let mut manager = ModelApiManager::default();
        manager
            .set_api(cfg("default-model", "k0"), ApiPurpose::RootAgent, true)
            .unwrap();
        // SubAgent was never assigned -> falls back to the default.
        assert_eq!(
            manager.get_api(ApiPurpose::SubAgent).unwrap().model,
            "default-model"
        );
    }

    #[test]
    fn unbound_purpose_without_default_is_none() {
        let mut manager = ModelApiManager::default();
        manager
            .set_api(cfg("root-model", "k1"), ApiPurpose::RootAgent, false)
            .unwrap();
        assert_eq!(manager.get_api(ApiPurpose::Compaction), None);
    }

    #[test]
    fn invalid_config_is_rejected_without_mutating_bindings() {
        let mut manager = ModelApiManager::default();
        let invalid = cfg("invalid", "");

        assert_eq!(
            manager.set_api(invalid, ApiPurpose::RootAgent, true),
            Err(InitError::MissingApiKey)
        );
        assert_eq!(manager.get_api(ApiPurpose::RootAgent), None);
    }

    #[test]
    fn setting_same_model_replaces_and_updates_all_bindings() {
        let mut manager = ModelApiManager::default();
        manager
            .set_api(cfg("shared", "old-key"), ApiPurpose::RootAgent, false)
            .unwrap();
        manager
            .set_api(cfg("shared", "old-key"), ApiPurpose::Memory, false)
            .unwrap();
        // Set the same model again with a rotated key.
        manager
            .set_api(cfg("shared", "new-key"), ApiPurpose::RootAgent, false)
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

    #[test]
    fn explicit_bindings_and_default_can_be_rebuilt_without_changing_fallbacks() {
        let mut manager = ModelApiManager::default();
        manager
            .set_api(cfg("default-model", "k0"), ApiPurpose::RootAgent, true)
            .unwrap();
        manager
            .set_api(cfg("memory-model", "k1"), ApiPurpose::Memory, false)
            .unwrap();

        let mut restored = ModelApiManager::default();
        restored
            .set_api(
                manager.get_explicit_api(ApiPurpose::RootAgent).unwrap(),
                ApiPurpose::RootAgent,
                false,
            )
            .unwrap();
        restored
            .set_api(
                manager.get_explicit_api(ApiPurpose::Memory).unwrap(),
                ApiPurpose::Memory,
                false,
            )
            .unwrap();
        restored
            .set_default_api(manager.get_default_api().unwrap())
            .unwrap();

        assert_eq!(
            restored.get_api(ApiPurpose::RootAgent).unwrap(),
            cfg("default-model", "k0")
        );
        assert_eq!(
            restored.get_api(ApiPurpose::Memory).unwrap(),
            cfg("memory-model", "k1")
        );
        assert_eq!(
            restored.get_api(ApiPurpose::Compaction).unwrap(),
            cfg("default-model", "k0")
        );
    }
}
