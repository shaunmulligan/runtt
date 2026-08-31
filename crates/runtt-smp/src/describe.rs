//! The `describe` command: our own SMP group.
//!
//! Runtime and firmware ship from different parties, so before pushing images at
//! a board the host needs to establish what it is talking to. This is what turns
//! version skew into a clear error instead of a timeout — and a board with no
//! contract at all into a legible one, rather than a silent port.
//!
//! Lives at `MGMT_GROUP_ID_PERUSER` (64), the first group id reserved for
//! applications. Only groups below 64 are guaranteed to be CBOR, so anything we
//! define here is ours to specify; see `docs/WIRE_CONTRACT.md`.

use anyhow::{Context, Result};
use mcumgr_toolkit::commands::McuMgrCommand;
use serde::{Deserialize, Serialize};

/// First group id reserved for application use (`MGMT_GROUP_ID_PERUSER`).
pub const GROUP_PERUSER: u16 = 64;
/// The only command in our group.
pub const CMD_DESCRIBE: u8 = 0;

/// What the firmware reports about itself.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Describe {
    /// Wire-contract version, so the host can refuse a mismatch outright.
    pub contract: String,
    /// `CONFIG_BOARD_TARGET`, e.g. `rpi_pico/rp2040`.
    pub board: String,
    /// The application's own version.
    pub app_version: String,
    /// 2 for the normal management + log split, 1 for single-serial targets.
    pub channels: u32,
    /// Whether the device implements the SMP image group, i.e. whether it can
    /// receive an update at all. Absent on contract 1.0.0 firmware, which
    /// predates the field -- treat that as "unknown", not "no".
    #[serde(default)]
    pub img: Option<bool>,

    /// True only for `runtt-idle`, the placeholder that ships in slot 0 at
    /// provisioning time. Absent on firmware predating the field.
    #[serde(default)]
    pub idle: Option<bool>,

    /// Present only when the firmware opted into liveness reporting, so the host
    /// can distinguish "healthy" from "does not report health".
    #[serde(default)]
    pub app_healthy: Option<bool>,

    /// Whether the board carries a valid identity record in flash. Absent on
    /// firmware predating the field -- treat that as "unknown", not "no".
    #[serde(default)]
    pub provisioned: Option<bool>,

    /// The board serial from its identity record, when one is assigned.
    #[serde(default)]
    pub serial: Option<String>,

    /// The CAN id the board is ACTUALLY answering on, which need not be the one
    /// the placement label named -- see the note in `flash.rs`.
    #[serde(default)]
    pub can_node_id: Option<u32>,
}

/// An empty CBOR map: the request carries no arguments.
#[derive(Debug, Default, Serialize)]
pub struct DescribeRequest {}

pub struct DescribeCommand {
    payload: DescribeRequest,
}

impl Default for DescribeCommand {
    fn default() -> Self {
        Self::new()
    }
}

impl DescribeCommand {
    pub fn new() -> Self {
        Self {
            payload: DescribeRequest::default(),
        }
    }
}

impl McuMgrCommand for DescribeCommand {
    type Payload = DescribeRequest;
    type Response = Describe;

    fn is_write_operation(&self) -> bool {
        false
    }
    fn group_id(&self) -> u16 {
        GROUP_PERUSER
    }
    fn command_id(&self) -> u8 {
        CMD_DESCRIBE
    }
    fn data(&self) -> &Self::Payload {
        &self.payload
    }
}

impl crate::ToolkitClient {
    /// Ask the device to describe itself.
    ///
    /// A device without our module answers `MGMT_ERR_ENOTSUP`, which surfaces
    /// here as an error — and that is the correct legible symptom for "this
    /// board has no firmware contract", rather than a timeout.
    pub fn describe(&self) -> Result<Describe> {
        self.raw(&DescribeCommand::new())
            .context("describe failed; does this firmware include the runtt module?")
    }
}
