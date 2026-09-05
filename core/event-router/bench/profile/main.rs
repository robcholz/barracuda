//! Deterministic host heap profiles for Event Router workloads.
//!
//! Run one scenario per process so retained memory and allocation totals stay
//! attributable to that workload.

#![allow(clippy::expect_used)]
#![allow(clippy::panic)]

use core::cell::Cell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};
use std::future::pending;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, EventRouter, JsonRpcSchema, JsonSchema,
    JsonWriter, RegisterContext, Router, RpcFrame, RpcLaneStorage, RpcMethod, RpcRegistry,
    RunContext, Unary, UnregisterContext, WorkflowClient,
};
use barracuda_platform_test::install_global_memory_vfs;
use barracuda_profile::dhat::{AllocationStats, HeapProfile};
use futures_lite::future::{block_on, poll_once};
use static_cell::StaticCell;

barracuda_profile::install_dhat_allocator!();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    RpcUnary,
    RouterLifecycle,
    WorkflowCatalog,
}

impl Scenario {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "rpc-unary" => Ok(Self::RpcUnary),
            "router-lifecycle" => Ok(Self::RouterLifecycle),
            "workflow-catalog" => Ok(Self::WorkflowCatalog),
            other => Err(format!(
                "unknown scenario `{other}`; expected rpc-unary, router-lifecycle, or workflow-catalog"
            )),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::RpcUnary => "rpc-unary",
            Self::RouterLifecycle => "router-lifecycle",
            Self::WorkflowCatalog => "workflow-catalog",
        }
    }
}

#[derive(Clone, Copy)]
struct Report {
    live: AllocationStats,
    after_drop: AllocationStats,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (scenario, output_file) = parse_args()?;
    prepare_output(&output_file)?;
    let report = match scenario {
        Scenario::RpcUnary => profile_rpc(&output_file),
        Scenario::RouterLifecycle => profile_router(&output_file),
        Scenario::WorkflowCatalog => profile_catalog(&output_file),
    };
    println!("scenario={}", scenario.name());
    println!("output={}", output_file.display());
    print_stats("live", report.live);
    print_stats("after_drop", report.after_drop);
    Ok(())
}

fn parse_args() -> Result<(Scenario, PathBuf), String> {
    // Cargo appends `--bench` when launching a custom benchmark target.
    let mut args = std::env::args()
        .skip(1)
        .filter(|argument| argument != "--bench");
    let scenario = Scenario::parse(args.next().as_deref().unwrap_or("rpc-unary"))?;
    let output_file = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| default_output(scenario));
    if let Some(extra) = args.next() {
        return Err(format!("unexpected argument `{extra}`"));
    }
    Ok((scenario, output_file))
}

fn default_output(scenario: Scenario) -> PathBuf {
    PathBuf::from("target")
        .join("profiles")
        .join("event-router")
        .join(format!("{}.dhat.json", scenario.name()))
}

fn prepare_output(output_file: &Path) -> std::io::Result<()> {
    if let Some(parent) = output_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn print_stats(label: &str, stats: AllocationStats) {
    println!("{label}.total_bytes={}", stats.total_bytes);
    println!("{label}.total_allocations={}", stats.total_allocations);
    println!("{label}.peak_bytes={}", stats.peak_bytes);
    println!("{label}.peak_allocations={}", stats.peak_allocations);
    println!("{label}.current_bytes={}", stats.current_bytes);
    println!("{label}.current_allocations={}", stats.current_allocations);
}

struct Echo;

impl RpcMethod for Echo {
    const ADDRESS: &'static str = "profile.echo";
    type Request = [u8; 8];
    type Response = [u8; 8];
    type Error = ();
    type Input = Unary;
    type Output = Unary;
}

fn profile_rpc(output: &Path) -> Report {
    const CALLS: u64 = 20_000;
    let profile = HeapProfile::start(output);
    static LANES: StaticCell<RpcLaneStorage<1, 8, 1>> = StaticCell::new();
    let lanes = LANES.init(RpcLaneStorage::new());
    let registry = RpcRegistry::new(lanes);
    registry
        .register::<Echo, _>(
            "system",
            |_context, request: RpcFrame<[u8; 8]>| async move { Ok(Ok(*request.view()?)) },
        )
        .expect("register echo");
    let client = registry.client();
    block_on(async {
        for value in 0..CALLS {
            let response = client
                .call::<Echo>(value.to_le_bytes())
                .expect("prepare call")
                .await
                .expect("finish call")
                .expect("method success");
            assert_eq!(response.view(), Ok(&value.to_le_bytes()));
            drop(response);
        }
    });
    let live = profile.stats();
    drop(client);
    drop(registry);
    let after_drop = profile.stats();
    drop(profile);
    Report { live, after_drop }
}

const ROUTER_FRAME: usize = 16;

#[derive(Default)]
struct Counts {
    registered: Cell<usize>,
    polled: Cell<usize>,
    unregistered: Cell<usize>,
}

struct PendingComponent {
    counts: Rc<Counts>,
}

impl Component<ROUTER_FRAME> for PendingComponent {
    fn name(&self) -> &'static str {
        "profile-pending"
    }

    fn register(
        &mut self,
        _context: &mut RegisterContext<'_, ROUTER_FRAME>,
    ) -> ComponentResult<()> {
        self.counts
            .registered
            .set(self.counts.registered.get().saturating_add(1));
        Ok(())
    }

    fn run<'a>(&'a mut self, _context: RunContext<ROUTER_FRAME>) -> ComponentFuture<'a> {
        Box::pin(core::future::poll_fn(move |_context| {
            self.counts
                .polled
                .set(self.counts.polled.get().saturating_add(1));
            Poll::Pending
        }))
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        self.counts
            .unregistered
            .set(self.counts.unregistered.get().saturating_add(1));
        Ok(())
    }
}

fn profile_router(output: &Path) -> Report {
    const COMPONENTS: usize = 1_000;
    let profile = HeapProfile::start(output);
    static LANES: StaticCell<RpcLaneStorage<1, ROUTER_FRAME, 0>> = StaticCell::new();
    let lanes = LANES.init(RpcLaneStorage::new());
    let mut router = Router::new(lanes);
    let counts = Rc::new(Counts::default());
    let mut ids = Vec::with_capacity(COMPONENTS);
    for _ in 0..COMPONENTS {
        ids.push(
            router
                .load(Box::new(PendingComponent {
                    counts: Rc::clone(&counts),
                }))
                .expect("load component"),
        );
    }
    assert!(block_on(poll_once(&mut router)).is_none());
    for id in ids.into_iter().rev() {
        router.unload(id).expect("unload component");
    }
    assert_eq!(counts.registered.get(), COMPONENTS);
    assert_eq!(counts.polled.get(), COMPONENTS);
    assert_eq!(counts.unregistered.get(), COMPONENTS);
    let live = profile.stats();
    drop(router);
    drop(counts);
    let after_drop = profile.stats();
    drop(profile);
    Report { live, after_drop }
}

const EVENT_FRAME: usize = 128;

struct CatalogSink;

impl JsonRpcSchema for CatalogSink {
    const ADDRESS: &'static str = "scale.sink";
    const REQUEST_SCHEMA: JsonSchema = JsonSchema::new("{}");
    const RESPONSE_SCHEMA: JsonSchema = JsonSchema::new("{}");
    const MAX_REQUEST_BYTES: usize = 2;
    const MAX_RESPONSE_BYTES: usize = 2;
}

#[derive(Default)]
struct CatalogState {
    done: Cell<bool>,
    failed: Cell<bool>,
}

struct CatalogLoader {
    documents: Vec<String>,
    state: Rc<CatalogState>,
}

impl Component<EVENT_FRAME> for CatalogLoader {
    fn name(&self) -> &'static str {
        "profile-catalog-loader"
    }

    fn register(&mut self, context: &mut RegisterContext<'_, EVENT_FRAME>) -> ComponentResult<()> {
        context.register_json::<CatalogSink, _>(
            "*",
            |_context, _request, response: JsonWriter| async move { response.write("{}").await },
        )
    }

    fn run<'a>(&'a mut self, context: RunContext<EVENT_FRAME>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let client = WorkflowClient::<EVENT_FRAME>::new(context.rpc().clone());
            for document in &self.documents {
                if client.load(document).await.is_err() {
                    self.state.failed.set(true);
                    break;
                }
            }
            self.state.done.set(true);
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

fn profile_catalog(output: &Path) -> Report {
    const WORKFLOWS: usize = 256;
    let profile = HeapProfile::start(output);
    let documents = (0..WORKFLOWS)
        .map(|index| {
            format!(
                r#"{{"id":"workflow-{index:04}","match":{{"event":"scale.*"}},"steps":[{{"call":"scale.sink"}}]}}"#
            )
        })
        .collect();
    static LANES: StaticCell<RpcLaneStorage<2, EVENT_FRAME, 2>> = StaticCell::new();
    block_on(install_global_memory_vfs()).expect("install global benchmark VFS");
    let lanes = LANES.init(RpcLaneStorage::new());
    let mut router = block_on(EventRouter::new(lanes)).expect("create router");
    let state = Rc::new(CatalogState::default());
    router
        .load(Box::new(CatalogLoader {
            documents,
            state: Rc::clone(&state),
        }))
        .expect("load catalog component");
    drive_until(&mut router, || state.done.get());
    assert!(!state.failed.get());
    assert_eq!(router.workflow_definitions().len(), WORKFLOWS);
    let live = profile.stats();
    drop(router);
    drop(state);
    let after_drop = profile.stats();
    drop(profile);
    Report { live, after_drop }
}

fn drive_until<const N: usize, const M: usize, const Q: usize>(
    router: &mut EventRouter<N, M, Q>,
    ready: impl Fn() -> bool,
) {
    let mut context = Context::from_waker(Waker::noop());
    for _ in 0..1_000_000 {
        match Pin::new(&mut *router).poll(&mut context) {
            Poll::Ready(result) => panic!("router terminated: {result:?}"),
            Poll::Pending if ready() => return,
            Poll::Pending => {}
        }
    }
    panic!("poll limit exceeded");
}
