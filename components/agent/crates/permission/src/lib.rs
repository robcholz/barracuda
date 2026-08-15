#![no_std]
//! `barracuda-agent-permission` — the tool-permission policy layer.
//!
//! A pure, `barracuda-agent-runtime`-independent crate that answers one question: *may this tool
//! call run?* It models a call as an [`Action`] (verb + optional [`Resource`] +
//! [`RiskClass`]), evaluates it through a [`PermissionPolicy`] into a
//! [`PermissionDecision`] (`Allow` / `Ask` / `Deny`), and — for the `Ask` path —
//! remembers the human's answer in a [`GrantStore`] so a retried call resolves
//! without asking twice (and cannot loop).
//!
//! Layering: this crate sits *below* `barracuda-agent-runtime`. It deliberately does not
//! reference `barracuda-agent-runtime` identity types (`AgentId` / `AgentKind`); a
//! [`PermissionRequest`] carries the acting agent as borrowed primitives, so the
//! dependency stays one-directional (`barracuda-agent-runtime` → `barracuda-agent-permission`).
//!
//! # Example
//!
//! ```
//! use barracuda_agent_permission::{
//!     Action, AskAtOrAbove, GrantStore, PermissionDecision, PermissionPolicy,
//!     PermissionRequest, PolicyChain, Resource, RiskClass,
//! };
//!
//! let policy = PolicyChain::new().with(AskAtOrAbove::new(RiskClass::Moderate));
//! let action = Action::new("write_file", RiskClass::Moderate)
//!     .with_resource(Resource::Path("/data/x".into()));
//! let request = PermissionRequest::new(&action);
//!
//! // First time: the policy asks for approval.
//! assert!(matches!(policy.evaluate(&request), PermissionDecision::Ask { .. }));
//!
//! // After a human approves, the grant short-circuits the next identical call.
//! let mut grants = GrantStore::new();
//! grants.grant(action.signature());
//! assert!(grants.lookup(&action.signature()).is_some());
//! ```

extern crate alloc;

mod action;
mod grant;
mod level;
mod policy;

pub use action::{Action, Resource, RiskClass};
pub use grant::{Grant, GrantStore};
pub use level::PermissionLevel;
pub use policy::{
    AllowAll, AskAtOrAbove, PermissionDecision, PermissionPolicy, PermissionRequest, PolicyChain,
};
