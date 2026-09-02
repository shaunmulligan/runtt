#!/usr/bin/env python3
"""Inspect a native_sim simulated-flash file.

The simulated flash is a plain host file (`--flash=<path>`), which is what makes
native_sim genuinely useful: you can check from outside the device whether an
upload really landed, rather than taking the device's word for it.

Partition offsets match Zephyr's stock native_sim devicetree.
"""
import argparse
import sys

# From zephyr/boards/native/native_sim/native_sim.dts.
PARTITIONS = [
    ("boot",    0x00000000, 0x0000C000),
    ("slot0",   0x0000C000, 0x00069000),
    ("slot1",   0x00075000, 0x00069000),
    ("scratch", 0x000DE000, 0x0001E000),
    ("storage", 0x000FC000, 0x00004000),
]

MCUBOOT_IMAGE_MAGIC = bytes.fromhex("3db8f396")
# MCUboot writes this at the end of a slot to mark its trailer valid.
BOOT_MAGIC = bytes.fromhex("77c295f360d2ef7f3552500f2cb67980")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("flash", help="path to the --flash file")
    ap.add_argument("--expect-image", metavar="FILE",
                    help="assert this signed image is present in slot 1")
    args = ap.parse_args()

    try:
        data = open(args.flash, "rb").read()
    except OSError as e:
        print(f"cannot read {args.flash}: {e}", file=sys.stderr)
        return 2

    print(f"{args.flash}: {len(data)} bytes\n")
    print(f"{'partition':<10} {'offset':>10} {'used':>10}  notes")
    print("-" * 62)
    for name, off, size in PARTITIONS:
        region = data[off:off + size]
        used = sum(1 for b in region if b != 0xFF)
        notes = []
        if region[:4] == MCUBOOT_IMAGE_MAGIC:
            notes.append("MCUboot image header")
        if BOOT_MAGIC in region[-64:]:
            notes.append("trailer magic set (marked)")
        if used == 0:
            notes.append("erased")
        print(f"{name:<10} {off:#010x} {used:>10}  {', '.join(notes)}")

    if args.expect_image:
        want = open(args.expect_image, "rb").read()
        got = data[0x00075000:0x00075000 + len(want)]
        print()
        if got == want:
            print(f"slot 1 matches {args.expect_image} byte for byte ({len(want)} bytes)")
        else:
            first = next((i for i, (a, b) in enumerate(zip(got, want)) if a != b), None)
            print(f"slot 1 does NOT match {args.expect_image}"
                  f"{f' (first difference at offset {first})' if first is not None else ''}")
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
