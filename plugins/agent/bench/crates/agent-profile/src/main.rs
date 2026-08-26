//! Deterministic host memory-profiling scenarios for the assembled agent.
//!
//! This is a profiling harness, not a benchmark: each invocation runs exactly
//! one scenario in a fresh process and writes a DHAT allocation profile.
//!
//! ```bash
//! cargo run --profile profiling -p barracuda-agent-profile -- agent-init
//! ```

use std::path::{Path, PathBuf};

use barracuda_agent_runtime::{AgentRuntime, ModelApiFactory, RuntimeStorageConfig};
use barracuda_model_api::ModelApi;
use barracuda_platform_test::{memory_vfs, NeverStack};
use barracuda_profile::dhat::{AllocationStats, HeapProfile};
use futures_lite::future::block_on;
use http_client::ClientFactory;

barracuda_profile::install_dhat_allocator!();

type ProfileAgentRuntime = AgentRuntime;
static NETWORK: NeverStack = NeverStack;

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

fn profile_agent_init(output_file: &Path) -> Result<AllocationStats, Box<dyn std::error::Error>> {
    let profile = HeapProfile::start(output_file);
    let llm_factory =
        ModelApiFactory::new(|| ModelApi::new(ClientFactory::from_network(&NETWORK, &NETWORK)));
    let (runtime, service) = ProfileAgentRuntime::new(
        block_on(memory_vfs())?,
        RuntimeStorageConfig {
            persistence_root: "/profile/agent-init".to_owned(),
            skill_roots: Vec::new(),
        },
        llm_factory,
    )?;

    // Finish while the runtime is alive: `current_bytes` then represents memory
    // retained by a fully initialized AgentRuntime.
    let stats = profile.finish();
    drop(runtime);
    drop(service);
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
