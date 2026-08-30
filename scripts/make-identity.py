#!/usr/bin/env python3
"""Build a balena-mcu identity record.

The record carries a board's CAN node id and serial, and lives at the start of
`storage_partition` -- outside both MCUboot slots, so a firmware update cannot
cost a board its address.

The point is that ONE firmware image serves a whole fleet. Without this the CAN
node id is a Kconfig symbol, which makes the built image board-specific, and
since firmware ships as an OCI image it makes the *service image* board-specific
too. Per-board images defeat deltas and multiply the registry.

The layout is contractual; docs/WIRE_CONTRACT.md carries the same table, and
firmware/balena-mcu/include/balena_mcu/identity.h is the other implementation.
Extend only by claiming reserved space and bumping the version.

    # write a record to a file, for a provisioning image
    ./scripts/make-identity.py --can-node-id 0x45 --serial arm-01 -o identity.bin

    # ...and flash it to a Feather's storage partition, which is at 0xf8000
    pyocd flash -t nrf52840 --base-address 0xf8000 identity.bin
"""
import argparse
import binascii
import struct
import sys

MAGIC = 0x616E6C62  # "blna" little-endian
VERSION = 1
SERIAL_LEN = 16
RECORD_LEN = 32
NO_NODE_ID = 0xFFFF

# A node owns three consecutive ids: requests, replies, console. Standard 11-bit
# identifiers only, so the top two are unusable as a base.
MAX_STD_ID = 0x7FF
IDS_PER_NODE = 3
MAX_NODE_ID = MAX_STD_ID - (IDS_PER_NODE - 1)

# Where the record goes on each supported board, for the flashing hints below.
STORAGE_BASE = {
    "rpi_pico": 0x101B0000,  # XIP-mapped; 0x1b0000 into flash
    "adafruit_feather_nrf52840": 0xF8000,
    "esp32s3_devkitc": 0x3B0000,
    "native_sim": 0xFC000,
}


def parse_id(text):
    """Accept 0x42 or 66, and refuse anything the firmware would refuse."""
    try:
        value = int(text, 16) if text.lower().startswith("0x") else int(text, 10)
    except ValueError:
        raise argparse.ArgumentTypeError(
            f"CAN node id {text!r} is not a number; expected e.g. 0x42 or 66"
        )
    if value < 0 or value > MAX_NODE_ID:
        raise argparse.ArgumentTypeError(
            f"CAN node id {text!r} ({value:#x}) is out of range. A node owns three "
            f"consecutive ids -- requests, replies on +1, console on +2 -- and only "
            f"standard 11-bit identifiers are used, so the ceiling is {MAX_NODE_ID:#x}."
        )
    return value


def build(can_node_id, serial):
    """Return the 32 bytes of a valid record."""
    serial_bytes = serial.encode("ascii") if serial else b""
    if len(serial_bytes) > SERIAL_LEN:
        raise SystemExit(
            f"serial {serial!r} is {len(serial_bytes)} bytes; the field is {SERIAL_LEN}"
        )
    serial_bytes = serial_bytes.ljust(SERIAL_LEN, b"\0")

    # magic, version, 3 pad, node id, 2 reserved, serial -- then the CRC over
    # exactly those 28 bytes.
    body = struct.pack(
        "<IB3xHH16s",
        MAGIC,
        VERSION,
        can_node_id if can_node_id is not None else NO_NODE_ID,
        0,
        serial_bytes,
    )
    assert len(body) == RECORD_LEN - 4, f"body is {len(body)} bytes"
    return body + struct.pack("<I", binascii.crc32(body) & 0xFFFFFFFF)


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument(
        "--can-node-id",
        type=parse_id,
        help="base CAN id; the board also answers on +1 and logs on +2",
    )
    ap.add_argument("--serial", help=f"board serial, up to {SERIAL_LEN} ASCII bytes")
    ap.add_argument("-o", "--output", required=True, help="file to write (32 bytes)")
    ap.add_argument(
        "--board",
        choices=sorted(STORAGE_BASE),
        help="print the flash address for this board after writing",
    )
    args = ap.parse_args()

    if args.can_node_id is None and not args.serial:
        ap.error("give at least one of --can-node-id or --serial")

    record = build(args.can_node_id, args.serial)
    with open(args.output, "wb") as f:
        f.write(record)

    node = f"{args.can_node_id:#x}" if args.can_node_id is not None else "unset"
    print(f"wrote {len(record)} bytes to {args.output}", file=sys.stderr)
    print(f"  can node id: {node}", file=sys.stderr)
    print(f"  serial:      {args.serial or '(none)'}", file=sys.stderr)
    if args.board:
        print(
            f"  flash to:    {STORAGE_BASE[args.board]:#x} on {args.board}",
            file=sys.stderr,
        )


if __name__ == "__main__":
    main()
