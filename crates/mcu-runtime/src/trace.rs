//! Invocation tracing — the phase-0 diagnostic.
//!
//! The riskiest unknown in this project is what the container engine actually
//! passes us and what actually reaches the annotations map. Guessing is how you
//! get opaque shim failures, so every invocation appends a JSONL record with the
//! full argv, cwd and (for `create`) the parsed spec.
//!
//! Enabled by setting `MCU_RUNTIME_TRACE` to a file path. Off by default, and
//! failures here are always swallowed: tracing must never break a container.

use serde_json::{json, Value};
use std::io::Write;

pub fn record(event: &str, extra: Value) {
    let Ok(path) = std::env::var("MCU_RUNTIME_TRACE") else {
        return;
    };
    let record = json!({
        "event": event,
        "pid": std::process::id(),
        "ppid": unsafe { libc::getppid() },
        "uid": unsafe { libc::getuid() },
        "argv": std::env::args().collect::<Vec<_>>(),
        "cwd": std::env::current_dir().ok().map(|p| p.display().to_string()),
        "extra": extra,
    });
    let _ = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(f, "{record}")
    })();
}
