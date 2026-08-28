//! Our annotation namespace.
//!
//! Placement arrives as an OCI annotation on the container spec. How it gets
//! there is not our concern — on balena the supervisor reads it from the
//! target-state API and passes it to the engine; locally it comes from
//! `docker run --annotation` or a runtime arg. Our side of the contract is only
//! that we read it out of the spec.

/// Required on the spec: the transport-prefixed placement label, e.g. `usb:3-6`.
pub const SPEC_TARGET: &str = "io.balena.mcu.target";

/// Optional on the spec: skip the upload if the device already runs this digest.
pub const SPEC_SKIP_IF_SAME: &str = "io.balena.mcu.skip-if-same-hash";

// Keys we write into `state.json`'s annotations map. That map doubles as the
// IPC channel between verb invocations, since each verb runs as a fresh
// process with no memory of the last one.
pub const STATE_TARGET: &str = "io.balena.mcu.target";
pub const STATE_FIRMWARE_PATH: &str = "io.balena.mcu.firmware-path";
