# Security

## Reporting a vulnerability

Please report security issues privately, via GitHub's **Report a vulnerability**
button on the Security tab, rather than opening a public issue.

Include what you were doing, what happened, and — if you can — whether it needs
physical access to the device, access to the host, or neither. That last
distinction matters most for triage here.

## What is in the trust model

runtt runs **outside** the container, invoked instead of runc. It is trusted host
software: it opens device nodes, writes firmware, and holds the occupancy claim.
The firmware container is not trusted and is never executed — its only role is to
carry an image and name it in the entrypoint.

The security property the design turns on is that **confirmation is only
reachable through the contract**. An image is uploaded to the inactive slot,
marked *test*, and confirmed only after it boots, enumerates and answers SMP. An
image that cannot do those things is reverted by MCUboot on the next reset. This
makes a bad update recoverable without physical access.

## Known limitations, stated plainly

**The signing key shipped here is public.** Everything in this repository is
signed with MCUboot's *published* development key
(`bootloader/mcuboot/root-rsa-2048.pem`). Any image signed with it will verify, so
**no trust root is enrolled** and image signing provides no authenticity guarantee
as shipped. This is appropriate for a bench and unfit for a fleet. Generating and
enrolling a real key is a prerequisite for any deployment, and on some boards
rotating it requires physical access — see [`PROVISIONING.md`](https://github.com/shaunmulligan/runtt-boards/blob/main/docs/PROVISIONING.md).

**The identity record is not signed.** The per-board record in flash (CAN node id,
serial) sits outside MCUboot's signed slots by design, so an update cannot cost a
board its address. It is not covered by any signature. Nothing in it is trusted
for anything but addressing, and an attacker able to write flash already has
better options than renumbering a node.

**A CAN bus is a shared broadcast medium with no authentication.** Any node can
send any identifier. runtt's management traffic on a CAN bus is no more protected
than anything else on that bus; treat bus access as equivalent to device access.

**Placement by USB port path is physical, not cryptographic.** `usb:3-6` names a
position, so re-cabling a hub changes which board a label reaches. Placement by
serial (`usb:feather-01`) is stable but is also just a string the board asserts.
Neither is an authentication mechanism.

## Supported versions

This project is pre-1.0 and has no release stream yet. Fixes land on the default
branch.
