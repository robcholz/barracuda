//! Multi-key LLM API configuration management.
//!
//! A system may hold several LLM API configs (different providers, keys, or
//! models) used for different purposes. [`ModelApiManager`] registers configs
//! against an [`ApiPurpose`] and resolves the right one per purpose, falling
//! back to a registered default.

use alloc::vec::Vec;
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

impl ApiPurpose {
    const COUNT: usize = 4;

    const fn slot(self) -> usize {
        match self {
            Self::RootAgent => 0,
            Self::SubAgent => 1,
            Self::Memory => 2,
            Self::Compaction => 3,
        }
    }
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
    /// Registered configs, one per model.
    configs: Vec<ModelApiConfig>,
    /// Index into `configs` bound to each purpose, by [`ApiPurpose::slot`].
    by_purpose: [Option<usize>; ApiPurpose::COUNT],
    /// Index into `configs` resolved for a purpose without a binding.
    default: Option<usize>,
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
        let index = self.store(api);
        if let Some(binding) = self.by_purpose.get_mut(purpose.slot()) {
            *binding = Some(index);
        }
        if default {
            self.default = Some(index);
        }
        Ok(())
    }

    /// Stores `api`, replacing the config of the same model, and returns its
    /// index.
    fn store(&mut self, api: ModelApiConfig) -> usize {
        if let Some(index) = self
            .configs
            .iter()
            .position(|stored| stored.model == api.model)
        {
            if let Some(stored) = self.configs.get_mut(index) {
                *stored = api;
            }
            return index;
        }
        self.configs.reserve_exact(1);
        self.configs.push(api);
        self.configs.len().saturating_sub(1)
    }

    fn config(&self, index: Option<usize>) -> Option<ModelApiConfig> {
        self.configs.get(index?).cloned()
    }

    fn binding(&self, purpose: ApiPurpose) -> Option<usize> {
        self.by_purpose.get(purpose.slot()).copied().flatten()
    }

    /// Resolve the config for `purpose`: its explicit binding if present,
    /// otherwise the default, otherwise `None`.
    #[must_use]
    pub fn get_api(&self, purpose: ApiPurpose) -> Option<ModelApiConfig> {
        self.config(self.binding(purpose).or(self.default))
    }

    /// Returns the API explicitly bound to `purpose`, excluding default fallback.
    #[must_use]
    pub fn get_explicit_api(&self, purpose: ApiPurpose) -> Option<ModelApiConfig> {
        self.config(self.binding(purpose))
    }

    /// Returns the configured default API.
    #[must_use]
    pub fn get_default_api(&self) -> Option<ModelApiConfig> {
        self.config(self.default)
    }

    /// Installs an API as the fallback without changing any purpose binding.
    ///
    /// # Errors
    ///
    /// Returns [`InitError`] without changing the manager when `api` is invalid.
    pub fn set_default_api(&mut self, api: ModelApiConfig) -> Result<(), InitError> {
        api.validate()?;
        self.default = Some(self.store(api));
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
