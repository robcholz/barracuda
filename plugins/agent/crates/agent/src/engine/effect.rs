//! Typed effects emitted by model-callable tools and reduced by AgentEngine.

use alloc::{collections::VecDeque, string::String, sync::Arc, vec::Vec};
use core::cell::RefCell;

/// A tool-level request that changes the current task boundary.
///
/// The protocol contains no concrete tool or context-provider semantics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentEffect {
    /// Finish the current task with the supplied assistant message.
    Finish { final_message: String },
    /// Yield one assistant message and wait for the next task input.
    Yield { message: String },
}

struct EffectQueue {
    effects: RefCell<VecDeque<AgentEffect>>,
}

/// Cloneable sending endpoint injected into tools that may affect the agent.
///
/// Emission is synchronous and the guard is never held across an await.
#[derive(Clone)]
pub(crate) struct AgentEffectEmitter {
    inner: Arc<EffectQueue>,
}

/// Unique receiving endpoint owned by AgentEngine.
///
/// Deliberately not `Clone`: AgentEngine is the only reducer of tool effects.
pub(crate) struct AgentEffectInbox {
    inner: Arc<EffectQueue>,
}

/// Create the split tool-to-agent effect channel.
pub(crate) fn agent_effect_channel() -> (AgentEffectEmitter, AgentEffectInbox) {
    let inner = Arc::new(EffectQueue {
        effects: RefCell::new(VecDeque::new()),
    });
    (
        AgentEffectEmitter {
            inner: Arc::clone(&inner),
        },
        AgentEffectInbox { inner },
    )
}

impl AgentEffectEmitter {
    pub(crate) fn emit(&self, effect: AgentEffect) {
        self.inner.effects.borrow_mut().push_back(effect);
    }
}

impl AgentEffectInbox {
    pub(crate) fn drain(&mut self) -> Vec<AgentEffect> {
        let mut effects = self.inner.effects.borrow_mut();
        effects.drain(..).collect()
    }

    pub(crate) fn clear(&mut self) {
        self.inner.effects.borrow_mut().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{agent_effect_channel, AgentEffect};

    #[test]
    fn cloned_emitters_feed_the_unique_inbox_in_order() {
        let (emitter, mut inbox) = agent_effect_channel();
        let second_emitter = emitter.clone();

        emitter.emit(AgentEffect::Yield {
            message: "first".to_owned(),
        });
        second_emitter.emit(AgentEffect::Finish {
            final_message: "second".to_owned(),
        });

        let effects = inbox.drain();
        assert_eq!(
            effects,
            vec![
                AgentEffect::Yield {
                    message: "first".to_owned(),
                },
                AgentEffect::Finish {
                    final_message: "second".to_owned(),
                },
            ]
        );
    }
}
