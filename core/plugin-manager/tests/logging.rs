//! Plugin lifecycle logging integration test.

#![allow(clippy::expect_used)]

use std::boxed::Box;
use std::sync::Mutex;

use barracuda_event_router::{EventRouter, RpcLaneStorage};
use barracuda_kv::MAX_CAPACITY;
use barracuda_platform_test::{install_global_memory_vfs, memory_partition};
use barracuda_plugin_manager::{Plugin, PluginManager};
use futures_lite::future::block_on;
use log::{LevelFilter, Log, Metadata, Record};

const FRAME_SIZE: usize = 64;

static CAPTURE: CaptureLogger = CaptureLogger {
    records: Mutex::new(Vec::new()),
};

struct CaptureLogger {
    records: Mutex<Vec<String>>,
}

impl Log for CaptureLogger {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &Record<'_>) {
        self.records
            .lock()
            .expect("lock captured records")
            .push(format!("{} {}", record.level(), record.args()));
    }

    fn flush(&self) {}
}

struct IdentifiedPlugin;

impl Plugin<FRAME_SIZE> for IdentifiedPlugin {
    fn id() -> &'static str {
        "identified"
    }
}

#[test]
fn manager_logs_every_plugin_registration_and_start_boundary() {
    log::set_logger(&CAPTURE).expect("install capture logger");
    log::set_max_level(LevelFilter::Trace);

    let mut manager = block_on(async {
        let partition = memory_partition(MAX_CAPACITY)
            .await
            .expect("create database partition");
        PluginManager::open(partition)
            .await
            .expect("open Plugin storage")
    });
    block_on(install_global_memory_vfs()).expect("install global test VFS");
    let lanes = Box::leak(Box::new(RpcLaneStorage::new()));
    let mut router =
        block_on(EventRouter::<4, FRAME_SIZE, 4>::new(lanes)).expect("create Event Router");

    manager
        .register(&mut router, IdentifiedPlugin)
        .expect("register Plugin");
    manager.start(&mut router).expect("start Plugin");

    let records = CAPTURE.records.lock().expect("lock captured records");
    assert!(records
        .iter()
        .any(|line| line == "INFO registering Plugin identified"));
    assert!(records
        .iter()
        .any(|line| line == "INFO registered Plugin identified"));
    assert!(records
        .iter()
        .any(|line| line == "INFO starting Plugin identified"));
    assert!(records
        .iter()
        .any(|line| line == "INFO started Plugin identified"));
}
