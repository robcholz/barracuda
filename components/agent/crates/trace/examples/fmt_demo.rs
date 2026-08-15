//! Eyeball the unified host log format: one line per source (`log` facade and
//! `tracing`), all rendered as `<L> (<ms>) <tag>: <msg>`.
//!
//! Run: `cargo run --example fmt_demo -p barracuda-agent-trace`
//! - piped (non-TTY) → plain text, ANSI auto-stripped by anstream;
//! - on a TTY → per-level colors (E red, W yellow, I green).

use barracuda_agent_trace::LevelFilter;

fn main() -> anyhow::Result<()> {
    // `Trace` defers filtering to the compile-time `log_max_*` ceiling.
    barracuda_agent_trace::init_logger(
        LevelFilter::Trace,
        barracuda_agent_trace::LogOutput::Stderr,
    )?;
    barracuda_agent_trace::init_tracing(
        barracuda_agent_trace::TracingConfig::default()
            .with_context_group_keys("run", ["system", "session", "turn", "agent", "iteration"]),
    )?;

    // `log` facade records (the path `barracuda-agent-memory` still uses).
    log::error!(target: "demo", "an error from log");
    log::warn!(target: "demo", "a warning from log");
    log::info!(target: "demo", "info from log");
    log::debug!(target: "demo", "debug from log");

    // `tracing` spans/events flow through the same backend, so they share the
    // format (the leading `<L> (<ms>) <tag>:` then the flat-tree `TR …` line).
    tracing::info_span!(
        "turn",
        run.system = "agent-system-demo",
        run.session = "session-1",
    )
    .in_scope(|| {
        tracing::info!(tool = "files", "calling tool via tracing");
    });

    Ok(())
}
