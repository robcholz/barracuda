//! Configuration, signup, verify, and resend through the WebServer, with the
//! scripted Inkbox API answering every upstream call in order.

use alloc::format;

use barracuda_captive_portal_plugin::EntryState;
use rusty_fork::rusty_fork_test;
use serde_json::{json, Value};

use super::*;

const API_KEY: &str = "ApiKey_once";
const IDENTITY_ID: &str = "eeee5555-0000-0000-0000-000000000001";
const MANUAL_CONFIG: &str = r#"{"api_key":"ApiKey_once","identity_id":"eeee5555-0000-0000-0000-000000000001","api_base":"http://10.0.0.1:9000"}"#;
const SIGNUP_RESPONSE: &str = r#"{"email_address":"barracuda-a1b2@inkboxmail.com","agent_handle":"barracuda-a1b2","api_key":"ApiKey_once","organization_id":"org-1","claim_status":"agent_unclaimed","human_email":"person@example.com","message":"Verification email sent"}"#;
const IDENTITY_RESPONSE: &str =
    r#"{"id":"eeee5555-0000-0000-0000-000000000001","agent_handle":"barracuda-a1b2"}"#;

/// Runs `scenario` against a new channel whose upstream replies are `steps`.
fn with_harness(steps: impl IntoIterator<Item = ScriptStep>, scenario: impl FnOnce(&Harness)) {
    with_gateway(steps, false, scenario);
}

/// As [`with_harness`], with the `inkbox` channel taken while `occupied`.
fn with_gateway(
    steps: impl IntoIterator<Item = ScriptStep>,
    occupied: bool,
    scenario: impl FnOnce(&Harness),
) {
    run(
        Setup {
            occupied,
            ..Setup::unconfigured(steps)
        },
        scenario,
    );
}

impl Harness {
    /// Stores [`MANUAL_CONFIG`] through the configuration route.
    fn configure(&self) {
        assert_eq!(self.post(CONFIG_API_PATH, MANUAL_CONFIG).status, 204);
    }

    fn sign_up(&self, body: &str) -> Reply {
        self.post(SIGNUP_API_PATH, body)
    }

    fn verify(&self, body: &str) -> Reply {
        self.post(VERIFY_API_PATH, body)
    }

    fn resend(&self) -> Reply {
        self.post(RESEND_API_PATH, "")
    }
}

fn body_of(request: &str) -> Value {
    let (_, body) = request.split_once("\r\n\r\n").expect("request body");
    serde_json::from_str(body).expect("request JSON")
}

fn request_line(request: &str) -> &str {
    request.split("\r\n").next().expect("request line")
}

fn signup_count(harness: &Harness) -> usize {
    harness
        .requests()
        .iter()
        .filter(|request| request_line(request) == "POST /api/v1/agent-signup HTTP/1.1")
        .count()
}

#[test]
fn stored_configuration_round_trips_every_field() -> Result<(), serde_json::Error> {
    let config = StoredConfiguration {
        api_key: "secret".into(),
        identity_id: "identity".into(),
        api_base: "https://inkbox.example".into(),
        signup: Some(SignupRecord {
            human_email: "person@example.com".into(),
            email_address: "agent@inkboxmail.com".into(),
            claim_status: "agent_unclaimed".into(),
        }),
    };

    let bytes = encode_configuration(&config)?;
    let restored = decode_configuration(&bytes)?;

    assert_eq!(restored.api_key, config.api_key);
    assert_eq!(restored.identity_id, config.identity_id);
    assert_eq!(restored.api_base, config.api_base);
    let signup = restored.signup.expect("signup record");
    assert_eq!(signup.human_email, "person@example.com");
    assert_eq!(signup.email_address, "agent@inkboxmail.com");
    assert_eq!(signup.claim_status, "agent_unclaimed");
    Ok(())
}

#[test]
fn configuration_stored_before_signup_records_still_decodes() -> Result<(), serde_json::Error> {
    let restored = decode_configuration(
        br#"{"api_key":"secret","identity_id":"identity","api_base":"https://inkbox.example"}"#,
    )?;
    assert_eq!(restored.api_key, "secret");
    assert!(restored.signup.is_none());
    Ok(())
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn signup_stores_the_resolved_identity_and_registers_the_channel() {
        with_harness(
            [
                ScriptStep::json(200, SIGNUP_RESPONSE),
                ScriptStep::json(200, IDENTITY_RESPONSE),
                ScriptStep::json(200, r#"{"message":{"id":"message-uuid"}}"#),
            ],
            |harness| {
                let reply = harness.sign_up(r#"{"email":" person@example.com "}"#);

                assert_eq!(reply.status, 200);
                assert_eq!(
                    reply.json,
                    json!({
                        "email_address": "barracuda-a1b2@inkboxmail.com",
                        "claim_status": "agent_unclaimed",
                    })
                );
                assert!(!reply.raw.contains(API_KEY));
                let stored = harness.stored_configuration().expect("configuration stored");
                assert_eq!(stored.api_key, API_KEY);
                assert_eq!(stored.identity_id, IDENTITY_ID);
                assert_eq!(stored.api_base, API_BASE);
                let get = harness.get();
                assert_eq!(get.status, 200);
                assert_eq!(get.json["configured"], json!(true));
                assert_eq!(get.json["mode"], json!("send_receive"));
                assert_eq!(
                    get.json["signup"],
                    json!({
                            "human_email": "person@example.com",
                            "email_address": "barracuda-a1b2@inkboxmail.com",
                            "claim_status": "agent_unclaimed",
                    })
                );
                assert!(!get.raw.contains(API_KEY));
                assert_ne!(harness.channel.entry_status().state, EntryState::Off);

                assert!(harness.send_text());
                let requests = harness.requests();
                assert_eq!(
                    request_line(&requests[0]),
                    "POST /api/v1/agent-signup HTTP/1.1"
                );
                assert_eq!(
                    body_of(&requests[0]),
                    json!({
                        "human_email": "person@example.com",
                        "note_to_human": SIGNUP_NOTE_TO_HUMAN,
                        "display_name": "Barracuda",
                        "harness": "barracuda",
                    })
                );
                assert!(!requests[0].contains("X-API-Key"));
                assert_eq!(
                    request_line(&requests[1]),
                    "GET /api/v1/identities/barracuda-a1b2 HTTP/1.1"
                );
                assert!(requests[1].contains("X-API-Key: ApiKey_once\r\n"));
                assert_eq!(
                    request_line(&requests[2]),
                    "POST /api/v1/imessage/messages?agent_identity_id=eeee5555-0000-0000-0000-000000000001 HTTP/1.1"
                );
                assert!(requests[2].contains("X-API-Key: ApiKey_once\r\n"));
            },
        );
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn signup_validation_failure_maps_to_invalid_request_and_stores_nothing() {
        with_harness(
            [ScriptStep::json(
                422,
                r#"{"detail":[{"loc":["body","human_email"],"msg":"value is not a valid email address","type":"value_error"}]}"#,
            )],
            |harness| {
                let reply = harness.sign_up(r#"{"email":"not-an-email"}"#);

                assert_eq!(reply.status, 400);
                assert_eq!(
                    reply.json,
                    json!({
                        "error": "invalid_request",
                        "message": "value is not a valid email address",
                    })
                );
                assert!(harness.stored_configuration().is_none());
                assert!(!harness.send_text());
            },
        );
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn signup_rejects_a_missing_email_without_calling_inkbox() {
        with_harness([], |harness| {
            assert_eq!(harness.sign_up("{}").status, 400);
            assert_eq!(harness.sign_up(r#"{"email":"  "}"#).status, 400);
            assert!(harness.requests().is_empty());
        });
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn unresolved_identity_keeps_the_signup_and_a_retry_resumes_it() {
        with_harness(
            [
                ScriptStep::json(200, SIGNUP_RESPONSE),
                ScriptStep::json(404, r#"{"detail":"Identity not found"}"#),
                ScriptStep::json(200, IDENTITY_RESPONSE),
            ],
            |harness| {
                let reply = harness.sign_up(r#"{"email":"person@example.com"}"#);

                assert_eq!(reply.status, 502);
                assert_eq!(
                    reply.json,
                    json!({
                        "error": "upstream_unavailable",
                        "message": "Identity not found",
                        "retry": true,
                    })
                );
                assert!(!reply.raw.contains(API_KEY));
                assert!(harness.stored_configuration().is_none());
                assert_eq!(harness.get().json["configured"], json!(false));

                let reply = harness.sign_up(r#"{"email":" person@example.com "}"#);

                assert_eq!(reply.status, 200);
                assert_eq!(
                    reply.json,
                    json!({
                        "email_address": "barracuda-a1b2@inkboxmail.com",
                        "claim_status": "agent_unclaimed",
                    })
                );
                assert_eq!(signup_count(harness), 1);
                let requests = harness.requests();
                assert_eq!(requests.len(), 3);
                assert_eq!(
                    request_line(&requests[2]),
                    "GET /api/v1/identities/barracuda-a1b2 HTTP/1.1"
                );
                let stored = harness.stored_configuration().expect("configuration stored");
                assert_eq!(stored.api_key, API_KEY);
                assert_eq!(stored.identity_id, IDENTITY_ID);
            },
        );
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn rejected_registration_keeps_the_signup_and_a_retry_resumes_it() {
        with_gateway(
            [
                ScriptStep::json(200, SIGNUP_RESPONSE),
                ScriptStep::json(200, IDENTITY_RESPONSE),
            ],
            true,
            |harness| {
                let reply = harness.sign_up(r#"{"email":"person@example.com"}"#);

                assert_eq!(reply.status, 422);
                assert_eq!(
                    reply.json,
                    json!({"error": "registration_failed", "retry": true})
                );
                assert!(harness.stored_configuration().is_none());
                assert_eq!(harness.channel.entry_status().state, EntryState::Off);

                harness.occupant.replace(None);
                let reply = harness.sign_up(r#"{"email":"person@example.com"}"#);

                assert_eq!(reply.status, 200);
                assert_eq!(reply.json["email_address"], "barracuda-a1b2@inkboxmail.com");
                // Neither signup nor the identity lookup is repeated.
                assert_eq!(harness.requests().len(), 2);
                assert_eq!(signup_count(harness), 1);
                assert_eq!(
                    harness.stored_configuration().expect("configuration stored").api_key,
                    API_KEY
                );
                assert_ne!(harness.channel.entry_status().state, EntryState::Off);
            },
        );
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn storage_failure_keeps_the_signup_and_a_retry_resumes_it() {
        // A key longer than one storage value makes every store fail.
        let api_key = format!("ApiKey_{}", "k".repeat(1100));
        let signup = SIGNUP_RESPONSE.replace(API_KEY, &api_key);
        with_harness(
            [
                ScriptStep::json(200, &signup),
                ScriptStep::json(200, IDENTITY_RESPONSE),
            ],
            |harness| {
                for _ in 0..2 {
                    let reply = harness.sign_up(r#"{"email":"person@example.com"}"#);

                    assert_eq!(reply.status, 500);
                    assert_eq!(reply.json, json!({"error": "storage", "retry": true}));
                    assert!(!reply.raw.contains("ApiKey_"));
                }
                assert_eq!(signup_count(harness), 1);
                assert_eq!(harness.requests().len(), 2);
                assert!(harness.stored_configuration().is_none());
            },
        );
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn a_kept_signup_is_not_reused_for_another_email() {
        with_gateway(
            [
                ScriptStep::json(200, SIGNUP_RESPONSE),
                ScriptStep::json(200, IDENTITY_RESPONSE),
                ScriptStep::json(200, SIGNUP_RESPONSE),
                ScriptStep::json(200, IDENTITY_RESPONSE),
            ],
            true,
            |harness| {
                let reply = harness.sign_up(r#"{"email":"person@example.com"}"#);
                assert_eq!(reply.status, 422);

                harness.occupant.replace(None);
                let reply = harness.sign_up(r#"{"email":"other@example.com"}"#);

                assert_eq!(reply.status, 200);
                assert_eq!(signup_count(harness), 2);
            },
        );
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn manual_configuration_rejected_by_the_gateway_is_registration_failed() {
        with_gateway([], true, |harness| {
            let reply = harness.post(CONFIG_API_PATH, MANUAL_CONFIG);

            assert_eq!(reply.status, 422);
            assert_eq!(reply.json, json!({"error": "registration_failed"}));
            assert!(harness.stored_configuration().is_none());
            assert_eq!(harness.get().json["configured"], json!(false));
        });
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn get_reports_configuration_without_secrets() {
        with_harness([], |harness| {
            let reply = harness.get();
            assert_eq!(reply.status, 200);
            assert_eq!(reply.json["configured"], json!(false));
            assert!(reply.json.get("signup").is_none());
            assert_eq!(harness.channel.entry_status().state, EntryState::Off);

            harness.configure();

            let reply = harness.get();
            assert_eq!(reply.status, 200);
            assert_eq!(
                reply.json,
                json!({"configured": true, "mode": "send_receive",
                       "receive": {"state": "starting", "slots": {"in_use": 1, "capacity": harness.capacity}},
                       "owners": {"count": 0}})
            );
            assert!(!reply.raw.contains(API_KEY));
            assert_ne!(harness.channel.entry_status().state, EntryState::Off);
        });
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn verify_submits_the_code_with_the_stored_key() {
        with_harness(
            [ScriptStep::json(
                200,
                r#"{"claim_status":"agent_claimed","organization_id":"org-2","message":"ok"}"#,
            )],
            |harness| {
                harness.configure();
                let reply = harness.verify(r#"{"code":"483921"}"#);

                assert_eq!(reply.status, 200);
                assert_eq!(reply.json, json!({"claim_status": "agent_claimed"}));
                // A manual configuration has no signup record to update.
                assert_eq!(harness.get().json["configured"], json!(true));
                assert!(harness.get().json.get("signup").is_none());
                let requests = harness.requests();
                assert_eq!(
                    request_line(&requests[0]),
                    "POST /api/v1/agent-signup/verify HTTP/1.1"
                );
                assert!(requests[0].contains("X-API-Key: ApiKey_once\r\n"));
                assert_eq!(
                    body_of(&requests[0]),
                    json!({"verification_code": "483921"})
                );
            },
        );
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn verify_updates_the_stored_claim_status_of_a_signup() {
        with_harness(
            [
                ScriptStep::json(200, SIGNUP_RESPONSE),
                ScriptStep::json(200, IDENTITY_RESPONSE),
                ScriptStep::json(
                    200,
                    r#"{"claim_status":"agent_claimed","organization_id":"org-2","message":"ok"}"#,
                ),
            ],
            |harness| {
                let reply = harness.sign_up(r#"{"email":"person@example.com"}"#);
                assert_eq!(reply.status, 200);

                let reply = harness.verify(r#"{"code":"483921"}"#);

                assert_eq!(reply.status, 200);
                assert_eq!(
                    harness.get().json["signup"],
                    json!({
                        "human_email": "person@example.com",
                        "email_address": "barracuda-a1b2@inkboxmail.com",
                        "claim_status": "agent_claimed",
                    })
                );
                assert_eq!(
                    harness.stored_configuration().expect("configuration stored").api_key,
                    API_KEY
                );
            },
        );
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn wrong_code_is_verification_failed_with_the_upstream_message() {
        with_harness(
            [ScriptStep::json(
                422,
                r#"{"detail":"Invalid verification code"}"#,
            )],
            |harness| {
                harness.configure();
                let reply = harness.verify(r#"{"code":"000000"}"#);

                assert_eq!(reply.status, 422);
                assert_eq!(
                    reply.json,
                    json!({"error": "verification_failed", "message": "Invalid verification code"})
                );
            },
        );
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn verify_rejects_a_malformed_code_without_calling_inkbox() {
        with_harness([], |harness| {
            harness.configure();
            for body in [
                r#"{"code":"12345"}"#,
                r#"{"code":"12345a"}"#,
                r#"{"otp":"123456"}"#,
            ] {
                assert_eq!(harness.verify(body).status, 400);
            }
            assert!(harness.requests().is_empty());
        });
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn verify_and_resend_without_a_signup_conflict() {
        with_harness([], |harness| {
            let verify = harness.verify(r#"{"code":"123456"}"#);
            let resend = harness.resend();

            assert_eq!(verify.status, 409);
            assert_eq!(verify.json["error"], "conflict");
            assert_eq!(resend.status, 409);
            assert_eq!(resend.json["error"], "conflict");
            assert!(harness.requests().is_empty());
        });
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn resend_asks_inkbox_with_the_stored_key() {
        with_harness(
            [
                ScriptStep::json(
                    200,
                    r#"{"claim_status":"agent_unclaimed","organization_id":"org-1","message":"sent"}"#,
                ),
                ScriptStep::json(429, r#"{"detail":"Please wait before resending"}"#),
            ],
            |harness| {
                harness.configure();
                let sent = harness.resend();
                let limited = harness.resend();

                assert_eq!(sent.status, 204);
                assert!(sent.raw.is_empty());
                assert_eq!(
                    limited.json,
                    json!({"error": "verification_failed", "message": "Please wait before resending"})
                );
                let requests = harness.requests();
                assert_eq!(
                    request_line(&requests[0]),
                    "POST /api/v1/agent-signup/resend-verification HTTP/1.1"
                );
                assert!(requests[0].contains("X-API-Key: ApiKey_once\r\n"));
            },
        );
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn unreachable_inkbox_is_upstream_unavailable() {
        with_harness(
            [ScriptStep::response(
                502,
                "text/html",
                b"<html>bad gateway</html>",
                usize::MAX,
            )],
            |harness| {
                let reply = harness.sign_up(r#"{"email":"person@example.com"}"#);

                assert_eq!(reply.status, 502);
                assert_eq!(reply.json["error"], "upstream_unavailable");
            },
        );
    }
}

rusty_fork_test! {
    /// Runs in a process of its own: the Gateway's receive buffers are
    /// static, so each test needs a fresh process to get them.
    #[test]
    fn every_route_rejects_other_methods() {
        with_harness([], |harness| {
            let paths = [
                CONFIG_API_PATH,
                SIGNUP_API_PATH,
                VERIFY_API_PATH,
                RESEND_API_PATH,
            ];
            for path in paths {
                for method in ["PUT", "DELETE", "PATCH"] {
                    let reply = harness.request(method, path, "");
                    assert_eq!(reply.status, 405, "{method} {path}");
                    assert_eq!(reply.json, json!({"error": "method_not_allowed"}));
                }
            }
            // Only `/status` answers GET; the configuration path takes changes only.
            for path in paths {
                let reply = harness.request("GET", path, "");
                assert_eq!(reply.status, 405, "GET {path}");
            }
        });
    }
}
