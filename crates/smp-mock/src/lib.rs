//! SMP mock: a deterministic SMP server for testing the client's error paths.
//!
//! Its purpose is fault injection, not fidelity. It exists to prove the runtime
//! does the right thing when a device misbehaves — which is most of the risk in
//! a firmware-update system, and exactly what hardware cannot reproduce on demand.

pub mod codec;
pub mod device;
pub mod faults;
pub mod server;
