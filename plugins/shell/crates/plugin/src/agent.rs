//! The `AgentStatus` capability: what the home screen shows of the Agent.

use alloc::borrow::ToOwned as _;
use alloc::string::String;
use core::cell::{Cell, RefCell};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use serde::Deserialize;

/// What the Agent is doing, as the home screen shows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// Waiting to be spoken to.
    #[default]
    Standby,
    /// Hearing the person; the question is what it heard so far.
    Listening,
    /// Working on the question.
    Thinking,
    /// Showing its answer, while it is read out.
    Answering,
}

/// The Agent's state on the home screen; published by the `shell` Plugin.
///
/// Whatever runs the device's voice conversation reports through it: the
/// person's question as it is heard, that the Agent is thinking, and its
/// answer. The system UI returns to standby nine seconds after an answer or
/// when the person taps it.
#[derive(Default)]
pub struct AgentStatus {
    state: Cell<AgentState>,
    question: RefCell<String>,
    answer: RefCell<String>,
    changed: Signal<NoopRawMutex, ()>,
}

impl AgentStatus {
    /// The Agent in standby.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reports the Agent's state, the question, and the answer.
    pub fn set(&self, state: AgentState, question: &str, answer: &str) {
        self.state.set(state);
        question.clone_into(&mut self.question.borrow_mut());
        answer.clone_into(&mut self.answer.borrow_mut());
        self.changed.signal(());
    }

    /// The Agent's state.
    #[must_use]
    pub fn state(&self) -> AgentState {
        self.state.get()
    }

    /// The question being heard or answered.
    #[must_use]
    pub fn question(&self) -> String {
        self.question.borrow().clone()
    }

    /// The answer being shown.
    #[must_use]
    pub fn answer(&self) -> String {
        self.answer.borrow().clone()
    }

    /// Waits until the state changes. One waiter, the system UI, is served.
    pub async fn changed(&self) {
        self.changed.wait().await;
    }
}
