mod common;
mod file_delete;
mod file_edit;
mod file_list;
mod file_move;
mod file_read;
mod file_write;

use barracuda_agent_plugin::tools::ToolGroup;
use barracuda_vfs::ScopedVfs;

pub(super) fn file_tools(filesystem: ScopedVfs) -> ToolGroup {
    ToolGroup::new(
        "file",
        true,
        [
            file_read::tool(filesystem.clone()),
            file_list::tool(filesystem.clone()),
            file_write::tool(filesystem.clone()),
            file_edit::tool(filesystem.clone()),
            file_move::tool(filesystem.clone()),
            file_delete::tool(filesystem),
        ],
    )
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use embassy_futures::block_on;

    use super::{common::filesystem, file_tools};

    #[test]
    fn tool_group_is_ready_for_agent_registration() -> Result<(), Box<dyn core::error::Error>> {
        block_on(async {
            let group = file_tools(filesystem().await?);
            assert_eq!(group.id(), "file");
            Ok::<_, Box<dyn core::error::Error>>(())
        })
    }
}
