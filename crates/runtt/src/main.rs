//! `runtt` — an OCI runtime that deploys MCU firmware instead of running
//! a container.
//!
//! Registered with a container engine as a runc-style binary:
//!
//! ```text
//! # /etc/docker/daemon.json
//! { "runtimes": { "runtt": { "path": "/usr/local/bin/runtt" } } }
//! ```
//!
//! The engine's shim invokes us with runc's CLI. We accept the full global flag
//! set it passes and **ignore what we don't need** — rejecting an unknown global
//! is how you get opaque shim failures.

mod annotations;
mod flash;
mod lock;
mod proxy;
mod state;
mod trace;
mod verbs;

use anyhow::Result;
use clap::Parser;
use liboci_cli::{GlobalOpts, StandardCmd};
use serde_json::json;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "runtt",
    version,
    about = "An OCI runtime that flashes MCU firmware over MCUmgr SMP",
    // Unknown flags from a future engine should not take a container down.
    disable_help_subcommand = true
)]
struct Cli {
    #[command(flatten)]
    global: GlobalOpts,

    /// Append a JSONL record of every invocation (argv, cwd, parsed spec) to
    /// this path. Diagnostic only. Pass it via daemon.json "runtimeArgs",
    /// since the engine does not forward a user shell's environment.
    #[arg(long, global = true, value_name = "PATH")]
    mcu_trace: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Command,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    #[command(flatten)]
    Standard(StandardCmd),

    /// Internal: the resident process the engine tracks as the container.
    #[command(hide = true)]
    Proxy {
        #[arg(long)]
        container_id: String,
        #[arg(long)]
        target: String,
        #[arg(long)]
        firmware: PathBuf,
        /// Upload even when the device already runs this digest.
        #[arg(long)]
        force_reflash: bool,
        /// Where the log channel is, when the transport cannot discover it.
        #[arg(long)]
        log_target: Option<String>,
    },
}

fn main() {
    let code = match real_main() {
        Ok(()) => 0,
        Err(e) => {
            // One line to stderr; the engine surfaces this in its own logs and
            // it is usually the only thing a user will see.
            eprintln!("runtt: {e:#}");
            trace::record("error", json!({ "error": format!("{e:#}") }));
            1
        }
    };
    std::process::exit(code);
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(&cli.global);
    trace::init(cli.mcu_trace.clone());
    trace::record("invoked", json!({}));

    let ctx = verbs::Ctx {
        root: state::state_root(cli.global.root.as_deref()),
    };

    match cli.cmd {
        Command::Standard(StandardCmd::Create(c)) => {
            verbs::create(&ctx, &c.container_id, &c.bundle, c.pid_file.as_deref())
        }
        Command::Standard(StandardCmd::Start(c)) => verbs::start(&ctx, &c.container_id),
        Command::Standard(StandardCmd::State(c)) => verbs::print_state(&ctx, &c.container_id),
        Command::Standard(StandardCmd::Kill(c)) => {
            verbs::kill(&ctx, &c.container_id, &c.signal, c.all)
        }
        Command::Standard(StandardCmd::Delete(c)) => verbs::delete(&ctx, &c.container_id, c.force),
        Command::Proxy {
            container_id,
            target,
            firmware,
            force_reflash,
            log_target,
        } => {
            let code = proxy::run(
                &container_id,
                &target,
                &firmware,
                !force_reflash,
                log_target.as_deref(),
            )?;
            std::process::exit(code);
        }
    }
}

/// Logs go to the file the engine names with `--log`, since our stdout belongs
/// to the container and stderr is not reliably captured.
fn init_logging(global: &GlobalOpts) {
    use tracing_subscriber::EnvFilter;

    let default = if global.debug { "debug" } else { "info" };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));

    let json = global.log_format.as_deref() == Some("json");

    // Appending to the engine's log file is best-effort: if we cannot open it,
    // fall back to stderr rather than failing the container.
    let target: Box<dyn std::io::Write + Send + 'static> = match global.log.as_deref() {
        Some(path) => match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(f) => Box::new(f),
            Err(_) => Box::new(std::io::stderr()),
        },
        None => Box::new(std::io::stderr()),
    };

    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::sync::Mutex::new(target));

    if json {
        let _ = builder.json().try_init();
    } else {
        let _ = builder.try_init();
    }
}
