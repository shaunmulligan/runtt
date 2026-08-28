//! Invocation tracing — the phase-0 diagnostic.
//!
//! The riskiest unknown in this project is what the container engine actually
//! passes us and what actually reaches the annotations map. Guessing is how you
//! get opaque shim failures, so every invocation appends a JSONL record with the
//! full argv, cwd and (for `create`) the parsed spec.
//!
//! Enabled by `--mcu-trace <path>` or the `MCU_RUNTIME_TRACE` environment
//! variable. The flag matters because a container engine invokes us from the
//! daemon's environment, not a user shell, so the env var alone is unreachable
//! there — pass it via `"runtimeArgs"` in daemon.json instead.
//!
//! Off by default, and failures here are always swallowed: tracing must never
//! break a container.

use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

static TRACE_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Called once at startup with the value of `--mcu-trace`, if given.
pub fn init(flag: Option<PathBuf>) {
    let resolved = flag.or_else(|| std::env::var_os("MCU_RUNTIME_TRACE").map(PathBuf::from));
    let _ = TRACE_PATH.set(resolved);
}

fn path() -> Option<&'static PathBuf> {
    TRACE_PATH.get().and_then(|o| o.as_ref())
}

pub fn record(event: &str, extra: Value) {
    let Some(path) = path() else {
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
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        // World-writable: the engine runs us as root, but a developer reading
        // the trace afterwards is not root. This file is a debug aid only.
        let _ = f.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o666));
        writeln!(f, "{record}")
    })();
}
