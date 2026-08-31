//! Container state persistence.
//!
//! Each verb runs as a fresh process, so everything the next verb needs must be
//! on disk. Writes are atomic (temp + rename) so a crash mid-write cannot leave
//! a half-parsed state file — and a stale PID must never be signalled.

use anyhow::{bail, Context, Result};
use oci_spec::runtime::{ContainerState, State};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Validate a container ID **before** joining it into any path.
///
/// Without this, a crafted id like `../../../etc` would be joined into the state
/// directory and opened. Borrowed from balena-extension-runtime, which hit the
/// same hazard.
pub fn validate_container_id(id: &str) -> Result<()> {
    if id.is_empty() {
        bail!("container id must not be empty");
    }
    if id.len() > 1024 {
        bail!("container id is too long ({} bytes, max 1024)", id.len());
    }
    let mut chars = id.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphanumeric() {
        bail!("container id {id:?} must start with an alphanumeric character");
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-') {
            bail!("container id {id:?} contains disallowed character {c:?}");
        }
    }
    Ok(())
}

/// Where state lives. `--root` when the engine supplies it (it does), else
/// `$XDG_RUNTIME_DIR/runtt`, else `/run/runtt`.
pub fn state_root(root_flag: Option<&Path>) -> PathBuf {
    if let Some(r) = root_flag {
        return r.to_path_buf();
    }
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("runtt");
        }
    }
    PathBuf::from("/run/runtt")
}

fn state_dir(root: &Path, id: &str) -> PathBuf {
    root.join(id)
}

fn state_file(root: &Path, id: &str) -> PathBuf {
    state_dir(root, id).join("state.json")
}

pub fn new_state(id: &str, bundle: &Path) -> State {
    let mut s = State::default();
    s.set_version(oci_spec::runtime::VERSION.to_string());
    s.set_id(id.to_string());
    s.set_status(ContainerState::Creating);
    s.set_bundle(bundle.to_path_buf());
    s.set_annotations(Some(HashMap::new()));
    s
}

pub fn write(root: &Path, state: &State) -> Result<()> {
    let id = state.id();
    validate_container_id(id)?;
    let dir = state_dir(root, id);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create state directory {}", dir.display()))?;

    let final_path = state_file(root, id);
    let tmp_path = final_path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(state).context("failed to serialise state")?;
    std::fs::write(&tmp_path, &json)
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    // Atomic: a reader sees either the old file or the new one, never a partial.
    std::fs::rename(&tmp_path, &final_path).inspect_err(|_| {
        // Do not leave a stray .tmp behind if the rename failed.
        let _ = std::fs::remove_file(&tmp_path);
    })?;
    Ok(())
}

pub fn read(root: &Path, id: &str) -> Result<State> {
    validate_container_id(id)?;
    let path = state_file(root, id);
    State::load(&path).map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))
}

pub fn remove(root: &Path, id: &str) -> Result<()> {
    validate_container_id(id)?;
    let dir = state_dir(root, id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("failed to remove {}", dir.display()))?;
    }
    Ok(())
}

/// Record one of our keys in the state annotations map. These are written for
/// observability — `docker inspect` and debugging — rather than read back.
pub fn set_annotation(state: &mut State, key: &str, value: String) {
    let mut map = state.annotations().clone().unwrap_or_default();
    map.insert(key.to_string(), value);
    state.set_annotations(Some(map));
}
