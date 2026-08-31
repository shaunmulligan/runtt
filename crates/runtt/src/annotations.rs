//! Our annotation namespace.
//!
//! Placement arrives as an OCI annotation on the container spec. How it gets
//! there is deliberately not our concern: locally it comes from `docker run
//! --annotation` or a runtime arg, and an orchestrator would pass it down from
//! its own target state. Our side of the contract is only that we read it out of
//! the spec, which is what lets the runtime work unchanged under any engine.

/// Required on the spec: the transport-prefixed placement label, e.g. `usb:3-6`.
pub const SPEC_TARGET: &str = "dev.runtt.target";

/// Optional on the spec: skip the upload if the device already runs this digest.
pub const SPEC_SKIP_IF_SAME: &str = "dev.runtt.skip-if-same-hash";

/// Optional on the spec: where the log channel is, for transports that cannot
/// discover it.
///
/// A `usb:` target finds both channels by interface string descriptor, so it
/// needs nothing. A `tty:` target names exactly one device, so there is nowhere
/// to put the second one -- which is the case for a simulator's pair of ptys and
/// for bring-up over a probe's UART bridge. Without this the log channel is
/// simply unreachable on those transports.
pub const SPEC_LOG_TARGET: &str = "dev.runtt.log-target";

// Keys we write into `state.json`'s annotations map. That map doubles as the
// IPC channel between verb invocations, since each verb runs as a fresh
// process with no memory of the last one.
pub const STATE_TARGET: &str = "dev.runtt.target";
pub const STATE_FIRMWARE_PATH: &str = "dev.runtt.firmware-path";
