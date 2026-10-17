//! Pure Agent tool groups.
//!
//! Tools owned by a context provider stay beside that provider. This module is
//! only for groups with no context-provider domain owner. Runtime features
//! such as multiagent are injected as ordinary `ToolGroup`s during construction.
//!
//! Human approval is **not** a tool: it is raised by the permission layer (an
//! `Ask` decision in `engine`), not requested or resolved by the model.
//!
mod background;
mod internal;

pub(crate) use background::background_tools;
pub(crate) use internal::internal_tools;
