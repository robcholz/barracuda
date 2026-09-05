//! Component lifecycle logging integration test.

#![allow(clippy::expect_used)]

use std::boxed::Box;
use std::future::pending;
use std::sync::Mutex;

use barracuda_router::{
    Component, ComponentFuture, ComponentResult, RegisterContext, Router, RunContext,
    UnregisterContext,
};
use barracuda_rpc::RpcLaneStorage;
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

struct NamedComponent;

impl Component<FRAME_SIZE> for NamedComponent {
    fn name(&self) -> &'static str {
        "named-probe"
    }

    fn register(&mut self, _context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, _context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(pending())
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

struct NamedCompletingComponent;

impl Component<FRAME_SIZE> for NamedCompletingComponent {
    fn name(&self) -> &'static str {
        "named-completing"
    }

    fn register(&mut self, _context: &mut RegisterContext<'_, FRAME_SIZE>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, _context: RunContext<FRAME_SIZE>) -> ComponentFuture<'a> {
        Box::pin(async { Ok(()) })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

#[test]
fn router_logs_component_name_and_runtime_identity() {
    log::set_logger(&CAPTURE).expect("install capture logger");
    log::set_max_level(LevelFilter::Trace);

    let lanes = Box::leak(Box::new(RpcLaneStorage::new()));
    let mut router = Router::<4, FRAME_SIZE, 4>::new(lanes);
    let id = router
        .load(Box::new(NamedComponent))
        .expect("load named Component");
    router.unload(id).expect("unload named Component");

    let lanes = Box::leak(Box::new(RpcLaneStorage::new()));
    let mut router = Router::<4, FRAME_SIZE, 4>::new(lanes);
    router
        .load(Box::new(NamedCompletingComponent))
        .expect("load named completing Component");
    let _error = futures_lite::future::block_on(&mut router)
        .expect_err("Component completion must terminate Router");

    let records = CAPTURE.records.lock().expect("lock captured records");
    assert!(records
        .iter()
        .any(|line| line == "INFO loaded Component named-probe (component-1)"));
    assert!(records
        .iter()
        .any(|line| line == "INFO unloaded Component named-probe (component-1)"));
    assert!(records.iter().any(|line| {
        line == "ERROR Event Router terminated: Component named-completing (component-1) exited while still loaded"
    }));
}
