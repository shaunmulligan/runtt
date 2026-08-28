//! SMP mock: speaks SMP over a pty and emulates the image-slot state machine.
//!
//! Its purpose is deterministic fault injection for the client's error paths,
//! not to be a device simulator. It lives in the test suite forever.

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).init();
    anyhow::bail!("smp-mock: not yet implemented (phase 0, step 3)")
}
