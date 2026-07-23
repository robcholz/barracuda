//! Deterministic host memory-profiling scenarios for the assembled agent.
//!
//! This is a profiling harness, not a benchmark: each invocation runs exactly
//! one scenario in a fresh process and writes a DHAT allocation profile.
//!
//! ```bash
//! cargo run --profile profiling -p claw-agent \
//!   --features memory_profile --example memory_profile -- agent-init
//! ```

use std::path::{Path, PathBuf};

use claw_agent::{AgentPersistenceConfig, AgentSystem};
use claw_interface::http::{
    Cancel, ClawHttp, HttpError, HttpJsonRequest, HttpResponseFuture, HttpStatusCode, SliceChunks,
    StreamingHttp,
};
use claw_interface::{ImmediateTimer, MemFs, StdThread, TokioExecutor};
use claw_profile::dhat::{AllocationStats, HeapProfile};

claw_profile::install_dhat_allocator!();

type ProfileAgentSystem = AgentSystem<MemFs, NeverHttp, ImmediateTimer>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    AgentInit,
}

impl Scenario {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "agent-init" => Ok(Self::AgentInit),
            other => Err(format!(
                "unknown scenario `{other}`; expected one of: agent-init"
            )),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::AgentInit => "agent-init",
        }
    }
}

#[derive(Default)]
struct NeverHttp;

impl ClawHttp for NeverHttp {
    fn post_json<'a>(
        &'a mut self,
        _request: &'a HttpJsonRequest<'a>,
        _cancel: Cancel<'a>,
    ) -> HttpResponseFuture<'a> {
        Box::pin(async { panic!("agent-init must not call HTTP") })
    }
}

impl StreamingHttp for NeverHttp {
    type ByteStream<'a>
        = SliceChunks<'a>
    where
        Self: 'a;

    async fn post_json_streaming<'a, 'r>(
        &'a mut self,
        _request: &'r HttpJsonRequest<'r>,
        _cancel: Cancel<'a>,
    ) -> Result<(HttpStatusCode, Self::ByteStream<'a>), HttpError> {
        panic!("agent-init must not call streaming HTTP")
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (scenario, output_file) = parse_args()?;
    prepare_output(&output_file)?;

    let stats = match scenario {
        Scenario::AgentInit => profile_agent_init(&output_file)?,
    };

    print_summary(scenario, &output_file, stats);
    Ok(())
}

fn parse_args() -> Result<(Scenario, PathBuf), String> {
    let mut args = std::env::args().skip(1);
    let scenario = Scenario::parse(args.next().as_deref().unwrap_or("agent-init"))?;
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
        .join(format!("{}.dhat.json", scenario.name()))
}

fn prepare_output(output_file: &Path) -> std::io::Result<()> {
    if let Some(parent) = output_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn profile_agent_init(output_file: &Path) -> Result<AllocationStats, claw_agent::AgentError> {
    MemFs::clear();
    let profile = HeapProfile::start(output_file);
    let system = ProfileAgentSystem::new::<StdThread, TokioExecutor>(AgentPersistenceConfig {
        persistence_root: "/profile/agent-init".to_owned(),
        skill_roots: Vec::new(),
    })?;

    // Finish while the system is alive: `current_bytes` then represents memory
    // retained by a fully initialized AgentSystem.
    let stats = profile.finish();
    drop(system);
    Ok(stats)
}

fn print_summary(scenario: Scenario, output_file: &Path, stats: AllocationStats) {
    println!("scenario={}", scenario.name());
    println!("output={}", output_file.display());
    println!("total_bytes={}", stats.total_bytes);
    println!("total_allocations={}", stats.total_allocations);
    println!("peak_bytes={}", stats.peak_bytes);
    println!("peak_allocations={}", stats.peak_allocations);
    println!("current_bytes={}", stats.current_bytes);
    println!("current_allocations={}", stats.current_allocations);
}
