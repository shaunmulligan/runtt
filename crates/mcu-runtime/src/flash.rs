//! The deploy sequence and the resident loop.
//!
//! The ordering here *is* the safety property, so it is written out explicitly:
//!
//! ```text
//! upload to the staging slot
//! mark it TEST            (never confirm yet)
//! reset
//! wait for it to enumerate, speak SMP and heartbeat
//! only then CONFIRM
//! ```
//!
//! Confirmation is therefore reachable only through the contract. An image that
//! removed or broke the contract can never be confirmed, because confirming
//! requires the very capability that was lost — so contract loss is never
//! remotely permanent. If we never send the confirm, MCUboot reverts on the next
//! reset by itself.

use anyhow::{bail, Context, Result};
use smp_client::{SmpClient, ToolkitClient};
use std::path::Path;
use std::time::{Duration, Instant};
use transport::resolve::Resolved;
use transport::usb::SerialChannel;

/// Baud is meaningless on CDC-ACM but must be something; 115200 is what a real
/// UART bring-up link will use.
const BAUD: u32 = 115_200;
const SMP_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for a board to come back after a reset.
const REBOOT_GRACE: Duration = Duration::from_secs(30);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// The wire-contract major version this runtime implements. A device reporting a
/// different major is refused rather than written to. See docs/WIRE_CONTRACT.md.
const CONTRACT_MAJOR: u32 = 1;
/// Describe is an optional probe, so it gets a short leash rather than the
/// patience an upload needs.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

pub struct Deploy<'a> {
    pub target: &'a str,
    pub firmware: &'a Path,
    pub resolved: Resolved,
    /// Skip the upload when the device already runs this exact digest, confirmed.
    pub skip_if_same: bool,
}

fn connect(path: &Path) -> Result<ToolkitClient> {
    let ch = SerialChannel::open(
        path.to_str().context("device path is not valid UTF-8")?,
        BAUD,
        SMP_TIMEOUT,
    )?;
    let c = ToolkitClient::new(ch, SMP_TIMEOUT)?;
    // Ask the device for its own buffer sizes rather than assuming Zephyr's
    // defaults; a mis-sized frame is a confusing mid-upload failure.
    c.tune_frame_size();
    Ok(c)
}

impl Deploy<'_> {
    /// Flash, verify and confirm. Returns a connected client for the resident loop.
    pub fn run(&self) -> Result<ToolkitClient> {
        let image = std::fs::read(self.firmware)
            .with_context(|| format!("failed to read firmware {}", self.firmware.display()))?;

        // Two different hashes, and they are not interchangeable. The upload's
        // `sha` field is over the file bytes (transfer integrity), but image
        // IDENTITY -- what `image list` reports and what `set_state` expects --
        // is the MCUboot digest over header and body only, from the image's TLV
        // area. Using the file hash for set_state yields
        // IMG_MGMT_ERR_HASH_NOT_FOUND.
        let info = smp_client::mcuboot::parse(&image).with_context(|| {
            format!(
                "{} is not a valid MCUboot image. A firmware service must ship an                  imgtool-signed image, not a raw binary.",
                self.firmware.display()
            )
        })?;
        let digest = info.digest;

        tracing::info!(
            target = self.target,
            bytes = image.len(),
            version = %info.version,
            digest = hex(&digest),
            "deploying firmware"
        );

        let mut c = connect(&self.resolved.mgmt)?;
        c.echo("balena")
            .context("the device did not answer an SMP echo; is the firmware contract present?")?;

        // Identify the device before writing anything to it. Placement is a USB
        // port path, which is physical rather than an identity: re-cable a hub
        // and the label still resolves, but now points at a different MCU. This
        // is the cheap check that stops us pushing nRF firmware to an RP2040.
        self.identify(&c)?;

        // Already running exactly this image, confirmed? Then there is nothing
        // to do, and reflashing would be a pointless write cycle plus a reboot.
        let slots = c.image_list()?;
        if self.skip_if_same {
            if let Some(active) = slots.iter().find(|s| s.active) {
                if active.hash.as_deref() == Some(digest.as_slice()) && active.confirmed {
                    println!("mcu: device already runs this digest, confirmed; nothing to do");
                    return Ok(c);
                }
            }
        }

        // Refuse to reflash a digest the device has already rejected. Without
        // this the restart policy turns a bad image into a reflash-revert storm.
        if slots
            .iter()
            .any(|s| s.hash.as_deref() == Some(digest.as_slice()) && !s.bootable)
        {
            bail!(
                "this exact image ({}) is already present and marked unbootable; \
                 refusing to reflash it. Push a different release.",
                hex(&digest)
            );
        }

        struct Log {
            last: Instant,
        }
        impl smp_client::Progress for Log {
            fn advance(&mut self, done: u64, total: u64) {
                // Throttle: a 200 KB image over 115200 baud is thousands of chunks.
                if self.last.elapsed() >= Duration::from_secs(2) || done == total {
                    let pct = (done * 100).checked_div(total).unwrap_or(0);
                    println!("mcu: uploading {done}/{total} bytes ({pct}%)");
                    self.last = Instant::now();
                }
            }
        }
        let mut progress = Log {
            last: Instant::now(),
        };

        c.flash(&image, Some(&mut progress))
            .context("firmware upload failed")?;

        // Cross-check what actually landed against what we sent. This catches a
        // corrupted upload before we ever mark it bootable, and it confirms the
        // device agrees with our own TLV parse.
        let staged = c
            .image_list()?
            .into_iter()
            .find(|s| !s.active && s.hash.as_deref() == Some(digest.as_slice()));
        if staged.is_none() {
            bail!(
                "after upload the device does not report an inactive image with                  digest {}. The upload did not land where expected.",
                hex(&digest)
            );
        }

        // Mark TEST, never confirm. This is the invariant.
        c.set_state(&digest, false)
            .context("failed to mark the uploaded image for test")?;
        println!("mcu: image staged and marked test, resetting");

        c.reset().context("failed to reset the device")?;
        drop(c);

        // The device is rebooting; the port may disappear and come back.
        let mut c = self
            .reconnect()
            .context("device did not come back after reset")?;

        let slots = c.image_list()?;
        let active = match slots.iter().find(|s| s.active) {
            Some(a) => a,
            None => {
                // Two very different situations, with opposite remedies.
                let staged_pending = slots
                    .iter()
                    .any(|s| s.hash.as_deref() == Some(digest.as_slice()) && s.pending);
                if staged_pending {
                    bail!(
                        "the image is staged and marked pending, but nothing swapped it in: \
                         no image is active after the reset. On a target with no bootloader \
                         this is expected and swap/confirm are unreachable by construction \
                         (native_sim cannot chain-load MCUboot). On real hardware it means \
                         MCUboot did not run -- check it is actually flashed, and that its \
                         swap mode matches the mode the image was built for."
                    );
                }
                bail!(
                    "no image is active after the reset, and our staged image is not \
                     pending either -- the upload appears to have been lost."
                );
            }
        };
        if active.hash.as_deref() != Some(digest.as_slice()) {
            bail!(
                "after reset the device is running digest {} but we deployed {}. \
                 The bootloader rejected or reverted the image.",
                active
                    .hash
                    .as_deref()
                    .map(hex)
                    .unwrap_or_else(|| "<none>".into()),
                hex(&digest)
            );
        }

        // It enumerated, spoke SMP and answered. Only now is confirming safe.
        c.set_state(&digest, true)
            .context("failed to confirm the running image")?;
        println!("mcu: image confirmed");
        Ok(c)
    }

    /// Query the device's own account of itself, and refuse an incompatible one.
    ///
    /// Firmware predating the describe command is tolerated with a warning: the
    /// img and os groups are standard MCUmgr, so such a device is still
    /// manageable, just unidentified. What is *not* tolerated is a device that
    /// answers describe with a contract major version we do not implement, since
    /// that is a positive statement of incompatibility rather than an absence.
    fn identify(&self, c: &ToolkitClient) -> Result<()> {
        // Fail fast. Firmware without the module never answers this, and making
        // every such deploy wait out the full upload timeout -- tens of seconds,
        // several retries -- is a poor trade for an optional probe.
        let d = {
            let _probe = c.probe_settings(PROBE_TIMEOUT, 1);
            c.describe()
        };
        let d = match d {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(
                    "device did not answer describe ({e:#}); proceeding unidentified. \
                     Board identity and contract version cannot be checked, so a \
                     mis-cabled target will not be caught."
                );
                return Ok(());
            }
        };

        let major = d
            .contract
            .split('.')
            .next()
            .and_then(|m| m.parse::<u32>().ok())
            .with_context(|| {
                format!(
                    "device reported an unparseable contract version {:?}",
                    d.contract
                )
            })?;
        if major != CONTRACT_MAJOR {
            bail!(
                "contract mismatch: the device speaks contract {} but this runtime \
                 implements major version {CONTRACT_MAJOR}. Refusing to write to it; \
                 update whichever side is older.",
                d.contract
            );
        }

        // Say so before attempting an upload that cannot work. Without this the
        // first sign of trouble is MGMT_ERR_ENOTSUP from the image group, which
        // names neither the cause nor the remedy.
        if d.img == Some(false) {
            bail!(
                "{} is running a bring-up configuration: it reports contract {} with no \
                 image management, so it can be identified and its logs read but it \
                 cannot receive firmware. That happens when the board has no secondary \
                 slot to stage into. Build for a target that has one -- for a Pico that \
                 is rpi_pico/rp2040/mcuboot under sysbuild rather than plain rpi_pico.",
                d.board,
                d.contract
            );
        }

        println!(
            "mcu: device is {} running {} (contract {}, {} channel{})",
            d.board,
            d.app_version,
            d.contract,
            d.channels,
            if d.channels == 1 { "" } else { "s" }
        );

        // A channel-count disagreement is not fatal, but it explains a silent
        // log channel, which is otherwise a confusing thing to chase.
        let resolved_channels = if self.resolved.log.is_some() { 2 } else { 1 };
        if d.channels != resolved_channels {
            tracing::warn!(
                device_reports = d.channels,
                host_resolved = resolved_channels,
                "channel count disagreement between the device and what the host resolved"
            );
        }

        if d.app_healthy == Some(false) {
            tracing::warn!(
                "the device reports its application thread as unhealthy before we have \
                 written anything to it"
            );
        }
        Ok(())
    }

    /// Poll for the device to reappear and answer, within the reboot grace period.
    fn reconnect(&self) -> Result<ToolkitClient> {
        let deadline = Instant::now() + REBOOT_GRACE;
        let mut last_err = None;
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(500));
            match connect(&self.resolved.mgmt).and_then(|mut c| {
                c.echo("balena")?;
                Ok(c)
            }) {
                Ok(c) => return Ok(c),
                Err(e) => last_err = Some(e),
            }
        }
        match last_err {
            Some(e) => Err(e).context("timed out waiting for the device to return"),
            None => bail!("timed out waiting for the device to return"),
        }
    }
}

/// Pipe the log channel to stdout and heartbeat the management channel until
/// something goes wrong.
///
/// Returning `Ok(())` means we were asked to stop. Returning `Err` means the
/// device went away — and the caller exits non-zero, which is what makes the
/// engine's restart policy fire.
pub fn stay_resident(
    mut c: ToolkitClient,
    log_channel: Option<&Path>,
    should_stop: &dyn Fn() -> bool,
) -> Result<()> {
    // The log channel is a plain byte stream: no framing, just whatever the
    // application printed. Pump it on its own thread straight to our stdout,
    // which containerd has already wired to the container's log.
    if let Some(path) = log_channel {
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            if let Err(e) = pump_logs(&path) {
                eprintln!("mcu-runtime: log channel closed: {e:#}");
            }
        });
    }

    let mut consecutive_failures = 0;
    loop {
        if should_stop() {
            tracing::info!("stopping on request");
            return Ok(());
        }
        std::thread::sleep(HEARTBEAT_INTERVAL);
        if should_stop() {
            return Ok(());
        }

        match c.echo("hb") {
            Ok(_) => consecutive_failures = 0,
            Err(e) => {
                consecutive_failures += 1;
                tracing::warn!("heartbeat {consecutive_failures} failed: {e:#}");
                // Two strikes: a single miss can be a transient USB hiccup, but
                // a second means the board is genuinely gone. Exiting non-zero
                // is the whole point — the restart policy takes it from here.
                if consecutive_failures >= 2 {
                    bail!("lost contact with the device after {consecutive_failures} heartbeats");
                }
            }
        }
    }
}

fn pump_logs(path: &Path) -> Result<()> {
    use std::io::{BufRead, BufReader};
    let ch = SerialChannel::open(
        path.to_str()
            .context("log device path is not valid UTF-8")?,
        BAUD,
        Duration::from_secs(3600),
    )?;
    let reader = BufReader::new(ch);
    for line in reader.lines() {
        match line {
            Ok(l) => println!("{l}"),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
