//! Build a one-step Workflow whose JSON RPC request selects fields from its Event input.

use barracuda_rpc::RpcAddress;
use barracuda_workflow::{Rule, WorkflowDefinition, WorkflowId, WorkflowStep};
use serde_json::json;

fn main() -> Result<(), Box<dyn core::error::Error>> {
    let arguments = json!({
        "prompt": "$event.input.message",
        "conversation": "$event.input.conversation",
        "mode": "chat"
    });
    let workflow = WorkflowDefinition::new(
        WorkflowId::try_from("incoming-message")?,
        Rule::try_from("gateway.message.received")?,
        vec![WorkflowStep::new(
            RpcAddress::try_from("agent.run")?,
            Some(arguments.clone()),
        )],
    )?;

    let [step] = workflow.steps() else {
        return Err("expected one Workflow step".into());
    };
    assert_eq!(step.address().as_ref(), "agent.run");
    assert_eq!(step.arguments(), Some(&arguments));
    Ok(())
}
