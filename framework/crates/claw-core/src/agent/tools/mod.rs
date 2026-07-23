//! Pure Agent tool groups.
//!
//! Tools owned by a context adapter stay beside that adapter. This module is
//! only for groups with no context-adapter domain owner. Runtime features
//! such as multiagent are injected as ordinary `ToolGroup`s during construction.
//!
//! Human approval is **not** a tool: it is raised by the permission layer (an
//! `Ask` decision in `base_agent`), not requested or resolved by the model.
//!
pub(crate) mod helper;
mod internal;

pub(in crate::agent) use internal::internal_tools;
