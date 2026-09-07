//! Agent-owned Workflow definitions loaded through the Workflow capability.

use alloc::vec::Vec;
use barracuda_plugin::manager::{PluginError, PluginResult};
use barracuda_vfs::ScopedVfs;
use barracuda_workflow_plugin::{
    WorkflowControlRejection, WorkflowService, WorkflowServiceError, WorkflowValue,
};

pub(super) async fn load(filesystem: &ScopedVfs, service: &WorkflowService) -> PluginResult<()> {
    let bytes = filesystem
        .read("/resources/workflows.json")
        .await
        .map_err(PluginError::registration)?;
    let documents: Vec<WorkflowValue> =
        serde_json::from_slice(&bytes).map_err(PluginError::registration)?;
    for document in documents {
        let json = serde_json::to_string(&document).map_err(PluginError::registration)?;
        match service.load_transient(&json).await {
            Ok(()) | Err(WorkflowServiceError::Rejected(WorkflowControlRejection::DuplicateId)) => {
            }
            Err(error) => return Err(PluginError::registration(error)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn bundled_catalog_remains_parse_compatible() {
        let documents: Vec<WorkflowValue> =
            serde_json::from_str(include_str!("../../../filesystem/resources/workflows.json"))
                .expect("bundled Workflow catalog");

        for document in documents {
            let json = serde_json::to_string(&document).expect("serialize Workflow document");
            barracuda_workflow_plugin::parse_definition(&json)
                .expect("parse bundled Workflow document");
        }
    }
}
