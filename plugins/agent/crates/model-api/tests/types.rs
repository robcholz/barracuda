use core::str::FromStr;

use barracuda_model_api::{
    BackendKind, HttpError, ModelApiError, ParseBackendKindError, RetryPolicy,
};

#[test]
fn backend_kind_string_contract_is_stable() {
    for (kind, id) in [
        (BackendKind::OpenAiCompatible, "openai_compatible"),
        (BackendKind::AnthropicCompatible, "anthropic_compatible"),
    ] {
        assert_eq!(kind.as_str(), id);
        assert_eq!(kind.to_string(), id);
        assert_eq!(BackendKind::from_str(id), Ok(kind));
    }

    let error = BackendKind::from_str("unknown").unwrap_err();
    let _: ParseBackendKindError = error;
    assert_eq!(error.to_string(), "unknown LLM backend type");
}

#[test]
fn retry_backoff_is_capped_and_saturating() {
    let policy = RetryPolicy::new(u32::MAX)
        .with_interval_ms(u32::MAX / 2)
        .with_multiplier(u32::MAX)
        .with_max_backoff_ms(u32::MAX - 1);

    assert_eq!(policy.backoff_ms(0), 0);
    assert_eq!(policy.backoff_ms(1), u32::MAX / 2);
    assert_eq!(policy.backoff_ms(2), u32::MAX - 1);
    assert_eq!(policy.backoff_ms(u32::MAX), u32::MAX - 1);
}

#[test]
fn transport_error_preserves_shared_http_source_and_classifies_retries() {
    let disconnected = HttpError::from(http_client::Error::ConnectionAborted);
    assert!(disconnected.retryable());
    assert!(matches!(
        disconnected,
        HttpError::Http(http_client::Error::ConnectionAborted)
    ));

    assert!(!HttpError::from(http_client::Error::InvalidUrl).retryable());
    assert!(!HttpError::Cancelled.retryable());
}

#[test]
fn api_error_delegates_transport_retry_classification() {
    let transient = ModelApiError::from(HttpError::from(http_client::Error::ConnectionAborted));
    assert!(transient.is_retryable());
    assert!(matches!(
        transient,
        ModelApiError::Transport(HttpError::Http(http_client::Error::ConnectionAborted))
    ));

    let permanent = ModelApiError::from(HttpError::from(http_client::Error::InvalidUrl));
    assert!(!permanent.is_retryable());
}
