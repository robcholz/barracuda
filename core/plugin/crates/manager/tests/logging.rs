//! Plugin lifecycle logging integration test.

#![allow(clippy::expect_used)]

use std::sync::Mutex;

use barracuda_kv::MAX_CAPACITY;
use barracuda_platform_test::memory_partition;
use barracuda_plugin_manager::{Plugin, PluginDeclaration, PluginManager};
use futures_lite::future::block_on;
use log::{LevelFilter, Log, Metadata, Record};

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

impl PluginDeclaration for IdentifiedPlugin {
    const ID: &'static str = "identified";
}

impl Plugin for IdentifiedPlugin {}

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
    manager.register(IdentifiedPlugin).expect("register Plugin");
    manager.start().expect("start Plugin");

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
