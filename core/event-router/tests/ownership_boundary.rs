#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use std::boxed::Box;

use barracuda_event_router::{EventRouter, RpcLaneStorage};
use barracuda_platform_test::install_global_memory_vfs;
use barracuda_vfs::{create_dir_all, read, write};
use futures_lite::future::block_on;

#[test]
fn event_router_owns_only_the_system_workflow_catalog() {
    block_on(async {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 64, 4>::new()));
        install_global_memory_vfs()
            .await
            .expect("install global test VFS");
        create_dir_all("/system")
            .await
            .expect("create shared System directory");
        write("/system/another-service.json", br#"{"owner":"other"}"#)
            .await
            .expect("write neighboring System state");

        EventRouter::new(lanes).await.expect("create Event Router");

        assert_eq!(
            read("/system/workflows.json")
                .await
                .expect("read Workflow catalog"),
            b"[]"
        );
        assert_eq!(
            read("/system/another-service.json")
                .await
                .expect("read neighboring System state"),
            br#"{"owner":"other"}"#
        );
    });
}
