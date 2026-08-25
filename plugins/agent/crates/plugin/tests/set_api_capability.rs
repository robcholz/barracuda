#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use std::cell::RefCell;
use std::rc::Rc;

use barracuda_agent_plugin::{AgentSetApi, ApiPurpose, BackendKind, InitError, ModelApiConfig};

fn config() -> ModelApiConfig {
    ModelApiConfig::new(
        BackendKind::OpenAiCompatible,
        "secret",
        "model",
        "https://example.invalid/v1",
    )
}

#[test]
fn capability_forwards_normal_model_api_types() {
    let observed = Rc::new(RefCell::new(None));
    let target = Rc::clone(&observed);
    let capability = AgentSetApi::new(move |api, purpose, default| {
        *target.borrow_mut() = Some((api, purpose, default));
        Ok(())
    });
    let api = config();

    assert_eq!(
        capability.set_api(api.clone(), ApiPurpose::RootAgent, true),
        Ok(())
    );
    assert_eq!(*observed.borrow(), Some((api, ApiPurpose::RootAgent, true)));
}

#[test]
fn capability_preserves_model_api_validation_errors() {
    let capability = AgentSetApi::new(|_api, _purpose, _default| Err(InitError::MissingApiKey));

    assert_eq!(
        capability.set_api(config(), ApiPurpose::RootAgent, true),
        Err(InitError::MissingApiKey)
    );
}
