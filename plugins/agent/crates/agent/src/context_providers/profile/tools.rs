//! Model-callable tools for editable profile documents.

use alloc::string::String;
use core::str::FromStr;

use barracuda_agent_memory::{ProfileDocument, ProfileStore};
use barracuda_agent_permission::{Action, Resource, RiskClass};
use barracuda_agent_tool::{
    tool_metadata, ToolContext, ToolError, ToolFuture, ToolHandler, ToolInvocation,
    ToolInvokeError, ToolOutput, ToolSpec,
};
use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct DocumentArgs {
    document: String,
}

#[derive(Deserialize)]
struct DocumentField {
    document: String,
}

#[derive(Deserialize)]
pub(super) struct ReplaceArgs {
    document: String,
    content: String,
}

pub(super) struct ProfileReadTool {
    pub(super) store: ProfileStore,
}

impl ToolSpec for ProfileReadTool {
    tool_metadata!("profile_read");

    fn concurrent(&self) -> bool {
        true
    }

    fn classify(&self, call: &ToolInvocation) -> Action {
        profile_action(call, "profile_read", RiskClass::Safe, &self.store)
    }
}

impl ToolHandler for ProfileReadTool {
    type Args = DocumentArgs;

    fn invoke<'a>(&'a self, _context: ToolContext, args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let document = parse_document(args.document)?;
            match self.store.read(document).await {
                Ok(Some(content)) => Ok(ToolOutput::new(
                    if content.trim().is_empty() {
                        format!("Profile document {document} is empty.")
                    } else {
                        format!("Profile document {document}:\n{content}")
                    },
                    true,
                )),
                Ok(None) => Ok(ToolOutput::new(
                    format!("Profile document {document} does not exist."),
                    true,
                )),
                Err(error) => Ok(ToolOutput::new(
                    format!("Could not read profile document {document}: {error}."),
                    false,
                )),
            }
        })
    }
}

pub(super) struct ProfileReplaceTool {
    pub(super) store: ProfileStore,
}

impl ToolSpec for ProfileReplaceTool {
    tool_metadata!("profile_replace");

    fn classify(&self, call: &ToolInvocation) -> Action {
        profile_action(call, "profile_replace", RiskClass::High, &self.store)
    }
}

impl ToolHandler for ProfileReplaceTool {
    type Args = ReplaceArgs;

    fn invoke<'a>(&'a self, _context: ToolContext, args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let document = parse_document(args.document)?;
            match self.store.replace(document, &args.content).await {
                Ok(()) => Ok(ToolOutput::new(
                    format!("Replaced profile document {document}."),
                    true,
                )),
                Err(error) => Ok(ToolOutput::new(
                    format!("Could not replace profile document {document}: {error}."),
                    false,
                )),
            }
        })
    }
}

pub(super) struct ProfileClearTool {
    pub(super) store: ProfileStore,
}

impl ToolSpec for ProfileClearTool {
    tool_metadata!("profile_clear");

    fn classify(&self, call: &ToolInvocation) -> Action {
        profile_action(call, "profile_clear", RiskClass::High, &self.store)
    }
}

impl ToolHandler for ProfileClearTool {
    type Args = DocumentArgs;

    fn invoke<'a>(&'a self, _context: ToolContext, args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let document = parse_document(args.document)?;
            match self.store.clear(document).await {
                Ok(()) => Ok(ToolOutput::new(
                    format!("Cleared profile document {document}."),
                    true,
                )),
                Err(error) => Ok(ToolOutput::new(
                    format!("Could not clear profile document {document}: {error}."),
                    false,
                )),
            }
        })
    }
}

fn profile_action(
    call: &ToolInvocation,
    verb: &str,
    risk: RiskClass,
    store: &ProfileStore,
) -> Action {
    let action = Action::new(verb, risk);
    let Ok(args) = call.arguments::<DocumentField>() else {
        return action;
    };
    let Ok(document) = parse_document(args.document) else {
        return action;
    };
    action.with_resource(Resource::Path(store.path(document)))
}

fn parse_document(document: String) -> Result<ProfileDocument, ToolInvokeError> {
    ProfileDocument::from_str(document.trim()).map_err(|error| {
        ToolInvokeError::new(ToolError::InvokeRejected(format!(
            "{error}; expected one of: soul, assistant_identity, user_profile"
        )))
    })
}

#[cfg(test)]
mod tests {
    use barracuda_agent_tool::ToolInvocation;

    use super::DocumentField;

    #[test]
    fn classification_can_project_document_from_replace_arguments(
    ) -> Result<(), Box<dyn core::error::Error>> {
        let call = ToolInvocation::try_new(
            None,
            "profile_replace",
            r#"{"document":"user_profile","content":"updated"}"#,
        )?;
        let args: DocumentField = call.arguments()?;
        assert_eq!(args.document, "user_profile");
        Ok(())
    }
}
