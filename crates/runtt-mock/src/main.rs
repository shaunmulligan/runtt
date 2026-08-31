//! SMP mock as a standalone process.
//!
//! Allocates a pty, prints the path a client should open, and serves SMP on it.
//! The announcement line deliberately mirrors native_sim's own
//! (`<name> connected to pseudotty: /dev/pts/N`) so one harness can drive both.
//!
//! Because pty numbers change on every run — and, on native_sim, on every reset
//! — prefer `--symlink` and have the client open a stable path.

use anyhow::{Context, Result};
use clap::Parser;
use runtt_mock::faults::Fault;
use runtt_mock::server::Server;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "runtt-mock",
    about = "A deterministic SMP server for testing error paths"
)]
struct Args {
    /// Which fault to inject.
    #[arg(long, value_enum, default_value = "none")]
    fault: FaultArg,

    /// For --fault disconnect-mid-upload: drop after this many chunks.
    #[arg(long, default_value = "2")]
    after_chunks: u32,

    /// For --fault restart-upload: demand a restart at this offset.
    #[arg(long, default_value = "512")]
    at_offset: u64,

    /// For --fault timeout: withhold responses for this group.
    #[arg(long, default_value = "0")]
    timeout_group: u16,

    /// For --fault timeout: withhold responses for this command id.
    #[arg(long, default_value = "0")]
    timeout_cmd: u8,

    /// Create a stable symlink to the allocated pty. Strongly recommended.
    #[arg(long)]
    symlink: Option<std::path::PathBuf>,

    /// Also emit application log output on the same link, making this a
    /// single-channel device. Use to exercise the runtime's log demux.
    #[arg(long)]
    chatter: Option<String>,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum FaultArg {
    None,
    DisconnectMidUpload,
    BadHash,
    RestartUpload,
    Timeout,
    RevertOnBoot,
    DigestAlreadyFailed,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let fault = match args.fault {
        FaultArg::None => Fault::None,
        FaultArg::DisconnectMidUpload => Fault::DisconnectMidUpload {
            after_chunks: args.after_chunks,
        },
        FaultArg::BadHash => Fault::BadHash,
        FaultArg::RestartUpload => Fault::RestartUpload {
            at_offset: args.at_offset,
        },
        FaultArg::Timeout => Fault::Timeout {
            group: args.timeout_group,
            cmd: args.timeout_cmd,
        },
        FaultArg::RevertOnBoot => Fault::RevertOnBoot,
        FaultArg::DigestAlreadyFailed => Fault::DigestAlreadyFailed,
    };

    // We hold the master; the client opens the slave, which is the end with a
    // real /dev/pts path.
    let (master, slave) = serialport::TTYPort::pair().context("failed to allocate a pty pair")?;
    let slave_path = {
        use serialport::SerialPort;
        slave.name().context("pty slave has no name")?
    };

    if let Some(link) = &args.symlink {
        let _ = std::fs::remove_file(link);
        std::os::unix::fs::symlink(&slave_path, link)
            .with_context(|| format!("failed to symlink {} -> {slave_path}", link.display()))?;
        println!("runtt-mock symlink: {} -> {slave_path}", link.display());
    }

    // Keep the slave fd open for the lifetime of the process: closing it would
    // tear down the pty before the client ever opens it.
    let _slave_keepalive = slave;

    println!("runtt-mock connected to pseudotty: {slave_path}");
    println!("runtt-mock fault: {fault:?}");
    use std::io::Write;
    std::io::stdout().flush().ok();

    let mut master = master;
    {
        use serialport::SerialPort;
        // A short timeout keeps the serve loop responsive to a closing peer.
        master.set_timeout(Duration::from_millis(50)).ok();
    }

    let mut server = Server::new(master, fault);
    if let Some(text) = args.chatter.as_deref() {
        server = server.with_chatter(text);
    }
    server.serve().context("SMP server loop failed")?;
    Ok(())
}
