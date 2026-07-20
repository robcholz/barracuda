use serde::{Deserialize, Serialize};

const PLAN_MODE_FRAMING: &str = prompt!("plan_mode/instructions.md");

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentMode {
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
