//! Typed effects emitted by model-callable tools and reduced by BaseAgent.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// A tool-level request that changes the current task boundary.
///
/// The protocol contains no concrete tool or context-adapter semantics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentEffect {
    /// Finish the current task with the supplied assistant message.
    Finish { final_message: String },
    /// Yield one assistant message and wait for the next task input.
    Yield { message: String },
}

struct EffectQueue {
    effects: Mutex<VecDeque<AgentEffect>>,
}

/// Cloneable sending endpoint injected into tools that may affect the agent.
///
/// Emission is synchronous and the guard is never held across an await.
#[derive(Clone)]
pub(in crate::agent) struct AgentEffectEmitter {
    inner: Arc<EffectQueue>,
}

/// Unique receiving endpoint owned by BaseAgent.
///
/// Deliberately not `Clone`: BaseAgent is the only reducer of tool effects.
pub(in crate::agent) struct AgentEffectInbox {
    inner: Arc<EffectQueue>,
}

/// Create the split tool-to-agent effect channel.
pub(in crate::agent) fn agent_effect_channel() -> (AgentEffectEmitter, AgentEffectInbox) {
    let inner = Arc::new(EffectQueue {
        effects: Mutex::new(VecDeque::new()),
    });
    (
        AgentEffectEmitter {
            inner: Arc::clone(&inner),
        },
        AgentEffectInbox { inner },
    )
}

impl AgentEffectEmitter {
    pub(in crate::agent) fn emit(&self, effect: AgentEffect) {
        self.inner
            .effects
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push_back(effect);
    }
}

impl AgentEffectInbox {
    pub(in crate::agent) fn drain(&mut self) -> Vec<AgentEffect> {
        let mut effects = self
            .inner
            .effects
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        effects.drain(..).collect()
    }

    pub(in crate::agent) fn clear(&mut self) {
        self.inner
            .effects
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clear();
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
