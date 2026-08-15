use alloc::{borrow::ToOwned, string::String, vec::Vec};

use serde::Deserialize;

const DEFAULT_RECALL_LIMIT: usize = 20;

#[derive(Deserialize)]
pub(super) struct StoreArgs {
    pub(super) content: String,
    #[serde(default)]
    pub(super) tags: Vec<String>,
    #[serde(default)]
    pub(super) keywords: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct RecallArgs {
    #[serde(default)]
    pub(super) labels: Vec<String>,
    pub(super) query: Option<String>,
    pub(super) limit: Option<usize>,
}

#[derive(Deserialize)]
pub(super) struct ListArgs {
    pub(super) limit: Option<usize>,
}

#[derive(Deserialize)]
pub(super) struct UpdateArgs {
    pub(super) id: String,
    pub(super) content: Option<String>,
    pub(super) tags: Option<Vec<String>>,
    pub(super) keywords: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub(super) struct IdArgs {
    pub(super) id: String,
}

pub(super) fn trimmed(value: String) -> String {
    value.trim().to_owned()
}

pub(super) fn optional_trimmed(value: Option<String>) -> Option<String> {
    value.map(trimmed)
}

pub(super) fn trimmed_strings(values: Vec<String>) -> Vec<String> {
    values.into_iter().map(trimmed).collect()
}

pub(super) fn optional_trimmed_strings(values: Option<Vec<String>>) -> Option<Vec<String>> {
    values.map(trimmed_strings)
}

pub(super) fn limit_or_default(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_RECALL_LIMIT)
}

#[cfg(test)]
mod tests {
    use barracuda_agent_tool::ToolInvocation;

    use super::StoreArgs;

    #[test]
    fn serde_projection_does_not_add_unknown_field_rules() -> Result<(), Box<dyn core::error::Error>>
    {
        let call = ToolInvocation::try_new(
            None,
            "memory_store",
            r#"{"content":"fact","unexpected":true}"#,
        )?;
        let args = call.arguments::<StoreArgs>()?;
        assert_eq!(args.content, "fact");
        Ok(())
    }
}
