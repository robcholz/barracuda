//! Host logging for framework tests and tools: the [`log`] facade backend and
//! the flat-tree `tracing` subscriber.
//!
//! The `log` facade is backed by [`env_logger`] with a custom format, and the
//! `tracing` sink re-enters the `log` facade, so both share one host sink.
//!
//! Output uses the familiar `<L> (<ms>) <tag>: <msg>` format and per-level
//! colors. Device applications should install a logger suited to their HAL.
//!
//! The two streams are independent (no `tracing/log-always`, no `LogTracer`), so
//! a `tracing` event never re-emits as a `log` record.
//!
//! Compile-time level ceilings are selected via the `log_max_*` / `trace_max_*`
//! Cargo features (see `Cargo.toml`); the runtime ceiling is [`init_logger`]'s
//! `max_level` argument (authoritative — `env_logger` does NOT read `RUST_LOG`),
//! and the compile-time ceiling selected by Cargo features.

mod subscriber;

use std::io;
use std::path::PathBuf;

use log::Level;
use thiserror::Error;
use tracing::Level as TraceLevel;

pub use log::LevelFilter;
pub use subscriber::{FlatTreeSubscriber, TraceSink};

/// Where the host `log` facade (and, through it, the `tracing` stream) writes.
///
#[derive(Debug, Clone, Default)]
pub enum LogOutput {
    /// Standard error — the default, unchanged behavior.
    #[default]
    Stderr,
    /// A file at this path, **truncated** (overwritten) on open. Written without
    /// ANSI color so the file stays plain text.
    File(PathBuf),
}

/// Failure installing the global `log` backend (see [`init_logger`]).
#[derive(Debug, Error)]
pub enum InitLoggerError {
    /// Opening the [`LogOutput::File`] target failed.
    #[error("failed to open log file {path}: {source}")]
    OpenLogFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A global `log` logger was already installed (`log` allows exactly one).
    #[error("a global logger is already installed")]
    SetLogger,
}

/// Install the host `env_logger` backend, capped at `max_level`.
///
/// `max_level` is the **runtime** gate for this firmware's own `barracuda*` targets.
/// It is `env_logger`'s authoritative filter (`RUST_LOG` is intentionally not
/// consulted) and layers under the compile-time `log_max_*` ceiling.
///
/// On host, dependencies that log through the `log` facade (TLS/network crates, …)
/// are capped at [`LevelFilter::Warn`] regardless of `max_level`, mirroring the
/// tracing target allowlist, so their verbose/debug output never floods the CLI.
///
/// Pass [`LevelFilter::Trace`] to defer all filtering to those other gates.
///
/// `output` selects the sink: [`LogOutput::Stderr`] (default) or
/// [`LogOutput::File`] to redirect log/trace output to a file. The interactive
/// CLI output (prompts/replies) is written
/// directly to stdout/stderr and is unaffected.
///
/// # Errors
///
/// Returns [`InitLoggerError::SetLogger`] if a global logger is already installed
/// (the `log` facade allows exactly one), or [`InitLoggerError::OpenLogFile`] if
/// the [`LogOutput::File`] target cannot be opened.
pub fn init_logger(max_level: LevelFilter, output: LogOutput) -> Result<(), InitLoggerError> {
    install_logger(max_level, output)
}

/// Per-level presentation: single letter (`E`/`W`/`I`/`D`/`V`) and ANSI color.
fn compact_style(level: Level) -> (char, Option<&'static str>) {
    match level {
        Level::Error => ('E', Some("31")), // red
        Level::Warn => ('W', Some("33")),  // yellow
        Level::Info => ('I', Some("32")),  // green
        Level::Debug => ('D', None),
        Level::Trace => ('V', None),
    }
}

fn install_logger(max_level: LevelFilter, output: LogOutput) -> Result<(), InitLoggerError> {
    use std::io::Write;
    use std::sync::OnceLock;
    use std::time::Instant;

    // Anchored at logger init so the `(ms)` column is process-relative uptime.
    static START: OnceLock<Instant> = OnceLock::new();

    let mut builder = env_logger::Builder::new();
    builder
        // No `parse_env`/`RUST_LOG`. Mirror the tracing target allowlist: noisy
        // dependencies (TLS/network crates emit through the `log` facade) only
        // surface at `Warn`+, while first-party `barracuda*` targets honor `max_level`.
        // So `init_logger(Trace)` keeps our verbose logs without dependency noise.
        .filter_level(LevelFilter::Warn)
        .filter_module(BARRACUDA_TARGET_PREFIX, max_level)
        // Render `<L> (<ms>) <tag>: <msg>`; anstream strips ANSI when the target
        // is not a TTY.
        .format(|formatter, record| {
            let uptime_ms = START.get_or_init(Instant::now).elapsed().as_millis();
            let (letter, color) = compact_style(record.level());
            let (tag, message) = (record.target(), record.args());
            match color {
                Some(code) => writeln!(
                    formatter,
                    "\x1b[0;{code}m{letter} ({uptime_ms}) {tag}: {message}\x1b[0m"
                ),
                None => writeln!(formatter, "{letter} ({uptime_ms}) {tag}: {message}"),
            }
        });

    // Redirect to a file when requested, forcing color off so the file stays
    // plain text; otherwise keep the default stderr target (auto color on a TTY).
    if let LogOutput::File(path) = output {
        let file = std::fs::File::create(&path)
            .map_err(|source| InitLoggerError::OpenLogFile { path, source })?;
        builder
            .target(env_logger::Target::Pipe(Box::new(file)))
            .write_style(env_logger::WriteStyle::Never);
    }

    builder.try_init().map_err(|_| InitLoggerError::SetLogger)?;
    Ok(())
}

/// Map a `tracing` level to the `log::Level` the sink expects.
fn to_log_level(level: TraceLevel) -> Level {
    match level {
        TraceLevel::ERROR => Level::Error,
        TraceLevel::WARN => Level::Warn,
        TraceLevel::INFO => Level::Info,
        TraceLevel::DEBUG => Level::Debug,
        TraceLevel::TRACE => Level::Trace,
    }
}

/// The `tracing` sink: forwards each already-formatted line to the same backend
/// as the `log` facade, so trace and `log` records share one output.
struct DefaultTraceSink;

impl TraceSink for DefaultTraceSink {
    fn write_line(&self, level: TraceLevel, tag: &str, line: &str) {
        log::log!(target: tag, to_log_level(level), "{line}");
    }
}

/// Target prefix for this framework's own crates (`barracuda_agent_runtime`, `barracuda_agent_tool`, …).
/// The subscriber traces only these, so dependency `tracing`
/// noise (TLS/network crates used by host CLIs) is dropped.
const BARRACUDA_TARGET_PREFIX: &str = "barracuda";

/// Caller-supplied configuration for [`init_tracing`].
///
/// `barracuda-agent-trace` bakes in no domain knowledge: the caller declares the
/// inherited-context groups (their names, keys, and order) here, keeping the
/// generic trace layer decoupled from agent-specific concepts. Build with
/// [`Default`] and layer groups via [`with_context_group_keys`].
///
/// ```
/// use barracuda_agent_trace::TracingConfig;
///
/// let config = TracingConfig::default()
///     .with_context_group_keys(
///         "run",
///         ["system", "session", "turn", "agent", "iteration"],
///     );
/// ```
///
/// [`with_context_group_keys`]: TracingConfig::with_context_group_keys
#[derive(Default)]
pub struct TracingConfig {
    context_groups: Vec<(&'static str, Vec<&'static str>)>,
}

impl TracingConfig {
    /// Register an inherited-context group `name` with its closed, ordered `keys`.
    ///
    /// A span field named `name.<key>` becomes this group's incremental context;
    /// see [`FlatTreeSubscriber::with_context_group_keys`].
    #[must_use]
    pub fn with_context_group_keys(
        mut self,
        name: &'static str,
        keys: impl IntoIterator<Item = &'static str>,
    ) -> Self {
        self.context_groups.push((name, keys.into_iter().collect()));
        self
    }
}

/// Install the flat-tree `tracing` subscriber. Its sink forwards to the same
/// backend as the `log` facade (`env_logger`).
///
/// Only spans/events whose `target` starts with `barracuda` are traced; dependency
/// output from third-party network crates is filtered out so it does not flood
/// the trace.
/// `config` declares the inherited-context groups (see [`TracingConfig`]).
///
/// Pair it with [`init_logger`] so plain `log::` records are emitted too; the
/// two streams are independent (no `tracing/log-always`, no `LogTracer`), so
/// there is no risk of a log<->trace loop.
///
/// # Errors
///
/// Returns [`tracing::subscriber::SetGlobalDefaultError`] if a global subscriber
/// is already installed (`tracing` allows exactly one).
pub fn init_tracing(
    config: TracingConfig,
) -> Result<(), tracing::subscriber::SetGlobalDefaultError> {
    let mut subscriber = FlatTreeSubscriber::with_sink(DefaultTraceSink)
        .with_allowed_target_prefix(BARRACUDA_TARGET_PREFIX);
    for (name, keys) in config.context_groups {
        subscriber = subscriber.with_context_group_keys(name, keys);
    }
    tracing::subscriber::set_global_default(subscriber)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn logger_file_sink_formats_first_party_levels_and_rejects_reinstallation() {
        let directory = tempfile::tempdir().expect("temporary directory creates");
        let path = directory.path().join("agent.log");

        init_logger(LevelFilter::Trace, LogOutput::File(path.clone()))
            .expect("logger installs once");
        log::error!(target: "barracuda_test", "failure");
        log::warn!(target: "barracuda_test", "warning");
        log::info!(target: "barracuda_test", "information");
        log::debug!(target: "barracuda_test", "diagnostic");
        log::trace!(target: "barracuda_test", "verbose");

        let contents = fs::read_to_string(path).expect("log file is readable");
        for (prefix, message) in [
            ("E (", "barracuda_test: failure"),
            ("W (", "barracuda_test: warning"),
            ("I (", "barracuda_test: information"),
            ("D (", "barracuda_test: diagnostic"),
            ("V (", "barracuda_test: verbose"),
        ] {
            assert!(
                contents.contains(prefix),
                "missing prefix {prefix}: {contents}"
            );
            assert!(
                contents.contains(message),
                "missing message {message}: {contents}"
            );
        }
        assert!(
            !contents.contains("\u{1b}["),
            "file output must not contain ANSI"
        );
        assert!(matches!(
            init_logger(LevelFilter::Info, LogOutput::Stderr),
            Err(InitLoggerError::SetLogger)
        ));
    }

    #[test]
    fn logger_reports_the_unopenable_file_path() {
        let directory = tempfile::tempdir().expect("temporary directory creates");
        let path = directory.path().join("missing").join("agent.log");
        match install_logger(LevelFilter::Info, LogOutput::File(path.clone())) {
            Err(InitLoggerError::OpenLogFile {
                path: failed_path, ..
            }) => assert_eq!(failed_path, path),
            other => panic!("expected file-open error, received {other:?}"),
        }
    }

    #[test]
    fn tracing_configuration_preserves_declared_group_order() {
        let config = TracingConfig::default()
            .with_context_group_keys("run", ["session", "turn"])
            .with_context_group_keys("tool", ["name"]);
        assert_eq!(
            config.context_groups,
            vec![("run", vec!["session", "turn"]), ("tool", vec!["name"])]
        );
    }

    #[test]
    fn level_mappings_cover_every_log_and_trace_level() {
        assert_eq!(compact_style(Level::Error), ('E', Some("31")));
        assert_eq!(compact_style(Level::Warn), ('W', Some("33")));
        assert_eq!(compact_style(Level::Info), ('I', Some("32")));
        assert_eq!(compact_style(Level::Debug), ('D', None));
        assert_eq!(compact_style(Level::Trace), ('V', None));

        assert_eq!(to_log_level(TraceLevel::ERROR), Level::Error);
        assert_eq!(to_log_level(TraceLevel::WARN), Level::Warn);
        assert_eq!(to_log_level(TraceLevel::INFO), Level::Info);
        assert_eq!(to_log_level(TraceLevel::DEBUG), Level::Debug);
        assert_eq!(to_log_level(TraceLevel::TRACE), Level::Trace);
    }
}
