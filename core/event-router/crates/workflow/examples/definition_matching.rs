//! Build Workflow definitions and match an Event in definition load order.

use barracuda_rpc::RpcAddress;
use barracuda_workflow::{
    EventId, Rule, WorkflowDefinition, WorkflowDefinitionError, WorkflowId, WorkflowStep,
};

fn definition(
    id: &str,
    event: &str,
    steps: &[&str],
) -> Result<WorkflowDefinition, Box<dyn core::error::Error>> {
    let steps = steps
        .iter()
        .map(|address| Ok(WorkflowStep::new(RpcAddress::try_from(*address)?, None)))
        .collect::<Result<Vec<_>, Box<dyn core::error::Error>>>()?;
    Ok(WorkflowDefinition::new(
        WorkflowId::try_from(id)?,
        Rule::try_from(event)?,
        steps,
    )?)
}

fn main() -> Result<(), Box<dyn core::error::Error>> {
    // Iteration order models the runtime catalog's definition load order.
    let definitions = [
        definition("all-gateway-messages", "gateway.*", &["agent.run"])?,
        definition(
            "received-message-audit",
            "gateway.message.received",
            &["audit.record"],
        )?,
        definition("scheduled-agent", "scheduler.*", &["agent.run"])?,
    ];
    let event_id = EventId::try_from("gateway.message.received")?;

    let matched: Vec<_> = definitions
        .iter()
        .filter(|workflow| workflow.event().matches(&event_id))
        .collect();

    assert_eq!(
        matched
            .iter()
            .map(|workflow| workflow.id().as_str())
            .collect::<Vec<_>>(),
        ["all-gateway-messages", "received-message-audit"]
    );
    assert_eq!(
        matched
            .first()
            .and_then(|workflow| workflow.steps().first())
            .map(|step| step.address().as_ref()),
        Some("agent.run")
    );
    assert_eq!(
        matched
            .get(1)
            .and_then(|workflow| workflow.steps().first())
            .map(|step| step.address().as_ref()),
        Some("audit.record")
    );

    let empty = WorkflowDefinition::new(
        WorkflowId::try_from("invalid-empty")?,
        Rule::try_from("gateway.*")?,
        Vec::new(),
    );
    assert!(matches!(empty, Err(WorkflowDefinitionError::EmptySteps)));

    println!("{} matched:", event_id.as_str());
    for workflow in matched {
        println!(
            "  {} -> {} RPC step(s)",
            workflow.id().as_str(),
            workflow.steps().len()
        );
    }
    Ok(())
}
