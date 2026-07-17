use serde::{Deserialize, Serialize};

const PLAN_MODE_FRAMING: &str = r#"<plan_mode>
You are in Plan Mode. Your job is to understand the request and produce an approved plan before implementation.

- Do not implement, edit files, or take the final requested action while Plan Mode is active.
- Ordinary tools remain available. Use them only to gather information needed to understand the task and form the plan.
- Resolve uncertainty by calling `plan_clarify` with one focused question. It yields that question and ends the current turn while Plan Mode remains active.
- The user's reply arrives as the next ordinary turn. Continue clarifying or revise the plan under the same Plan Mode framing.
- When the plan is ready, call `plan_clarify` to present the proposed plan and ask for explicit approval.
- Interpret approval semantically; do not require the exact word "ok".
- After approval, call `plan_exit` with `outcome` set to `execute` and include the complete final plan. Execution begins on the following iteration.
- If the user rejects the plan and does not want a revision, call `plan_exit` with `outcome` set to `cancel` and a short closing message. No plan is executed.
- Call a Plan Mode control tool by itself in its tool-call round.
</plan_mode>"#;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub(super) enum AgentMode {
    #[default]
    Normal,
    Plan,
}

impl AgentMode {
    pub(super) fn framing(self) -> &'static str {
        match self {
            Self::Normal => "",
            Self::Plan => PLAN_MODE_FRAMING,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AgentMode;

    #[test]
    fn only_plan_mode_contributes_mode_framing() {
        assert!(AgentMode::Normal.framing().is_empty());
        let framing = AgentMode::Plan.framing();
        assert!(framing.contains("Do not implement"));
        assert!(framing.contains("Ordinary tools remain available"));
        assert!(framing.contains("plan_exit"));
    }
}
