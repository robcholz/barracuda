//! Deterministic host memory-profiling scenarios for the assembled agent.
//!
//! This is a profiling harness, not a benchmark: each invocation runs exactly
//! one scenario in a fresh process and writes a DHAT allocation profile.
//!
//! ```bash
//! cargo run --profile profiling -p barracuda-agent-profile -- agent-init
//! ```

use std::path::{Path, PathBuf};
use std::{collections::BTreeMap, io};

use barracuda_agent_runtime::{AgentRuntime, ModelApiFactory, RuntimeStorageConfig};
use barracuda_model_api::{BackendKind, ChatRequest, ModelApi, ModelApiConfig};
use barracuda_platform_test::{memory_vfs, NeverStack, ScriptStep, ScriptedStack};
use barracuda_profile::dhat::{AllocationStats, HeapProfile};
use barracuda_runtime_utils::Cancel;
use base64::Engine as _;
use futures_lite::future::block_on;
use futures_lite::StreamExt;
use http_client::ClientFactory;

barracuda_profile::install_dhat_allocator!();

type ProfileAgentRuntime = AgentRuntime;
static NETWORK: NeverStack = NeverStack;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    AgentInit,
    TapeReplay,
}

impl Scenario {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "agent-init" => Ok(Self::AgentInit),
            "tape-replay" => Ok(Self::TapeReplay),
            other => Err(format!(
                "unknown scenario `{other}`; expected one of: agent-init, tape-replay"
            )),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::AgentInit => "agent-init",
            Self::TapeReplay => "tape-replay",
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (scenario, output_file) = parse_args()?;
    prepare_output(&output_file)?;

    let stats = match scenario {
        Scenario::AgentInit => profile_agent_init(&output_file)?,
        Scenario::TapeReplay => profile_tape_replay(&output_file)?,
    };

    print_summary(scenario, &output_file, stats);
    Ok(())
}

fn profile_tape_replay(output_file: &Path) -> Result<AllocationStats, Box<dyn std::error::Error>> {
    let profile = HeapProfile::start(output_file);
    let responses = load_tape_responses(Path::new("bench/tapes/overall-capabilities.jsonl"))?;
    let steps = responses
        .iter()
        .map(|body| ScriptStep::response(200, "text/event-stream", body, 7));
    let network = ScriptedStack::new(steps);
    let mut api = ModelApi::new(ClientFactory::from_network(&network, &network));
    api.set_config(ModelApiConfig::new(
        BackendKind::OpenAiCompatible,
        "replay-key",
        "replay-model",
        "http://tape.invalid",
    ))?;

    block_on(async {
        for turn in 0..responses.len() {
            let messages = [serde_json::json!({
                "role": "user",
                "content": format!("deterministic replay turn {turn}"),
            })];
            let request = ChatRequest::new("profiling replay", &messages);
            let mut stream = api.chat_stream(&request, Cancel::never()).await?;
            while let Some(event) = stream.next().await {
                let _event = event?;
            }
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    Ok(profile.finish())
}

fn load_tape_responses(path: &Path) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
    let mut responses = BTreeMap::<String, Vec<u8>>::new();
    for line in std::fs::read_to_string(path)?.lines() {
        let record: serde_json::Value = serde_json::from_str(line)?;
        match record.get("kind").and_then(serde_json::Value::as_str) {
            Some("response_start") => {
                responses.insert(interaction_id(&record)?.to_owned(), Vec::new());
            }
            Some("response_chunk") => {
                let encoded = record
                    .get("data_b64")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("response chunk has no data_b64")?;
                let decoded = base64::engine::general_purpose::STANDARD.decode(encoded)?;
                responses
                    .get_mut(interaction_id(&record)?)
                    .ok_or_else(|| invalid_data("response chunk precedes response_start"))?
                    .extend_from_slice(&decoded);
            }
            _ => {}
        }
    }
    Ok(responses.into_values().collect())
}

fn interaction_id(record: &serde_json::Value) -> Result<&str, io::Error> {
    record
        .get("interaction_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid_data("response record has no interaction_id"))
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
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
