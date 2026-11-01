#!/usr/bin/env python3
"""Extract small fuzz inputs from the eight attributed, fixed MP4 fixtures.

This is a fixture-only sample-table reader. It neither accepts arbitrary media
nor invokes an external decoder. --check verifies the committed seed bytes.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import struct

ROOT = Path(__file__).resolve().parents[1]


def module(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / ("tools/" + name + ".py"))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


BOXES = module("container_source_window")
FIXTURES = module("codec_contract").FIXTURES


def extract(fixture):
    data = (ROOT / fixture["path"]).read_bytes()
    if len(data) > 1024 * 1024 or hashlib.sha256(data).hexdigest() != fixture["sha256"]:
        raise ValueError("fixture-identity-invalid")
    mdia = BOXES.video(data)[2]
    stbl = BOXES.one(BOXES.one(mdia, b"minf"), b"stbl")
    description = BOXES.one(stbl, b"stsd")
    if description[:8] != b"\0\0\0\0\0\0\0\1":
        raise ValueError("fixture-description-invalid")
    entry, = BOXES.boxes(description[8:])
    codec, payload = entry
    config = BOXES.one(payload[78:], b"hvcC" if fixture["codec"] == "hevc" else b"vpcC")
    sizes = BOXES.one(stbl, b"stsz")
    default, count = struct.unpack_from(">II", sizes, 4)
    if count != 240:
        raise ValueError("fixture-count-invalid")
    sizes = (
        [default] * count
        if default
        else list(struct.unpack_from(">" + "I" * count, sizes, 12))
    )
    chunks = BOXES.one(stbl, b"stco")
    chunks = struct.unpack_from(">" + "I" * BOXES.integer(chunks, 4), chunks, 8)
    mapping = BOXES.one(stbl, b"stsc")
    entries = [
        struct.unpack_from(">III", mapping, 8 + index * 12)
        for index in range(BOXES.integer(mapping, 4))
    ]
    sync = BOXES.one(stbl, b"stss")
    sync = set(struct.unpack_from(">" + "I" * BOXES.integer(sync, 4), sync, 8))
    packets = []
    for ordinal, offset in enumerate(chunks, 1):
        selected = [row for row in entries if row[0] <= ordinal][-1]
        if selected[2] != 1:
            raise ValueError("fixture-description-invalid")
        for _ in range(selected[1]):
            size = sizes[len(packets)]
            packet = data[offset:offset + size]
            if len(packet) != size:
                raise ValueError("fixture-range-invalid")
            packets.append((len(packets) + 1 in sync, packet))
            offset += size
    if len(packets) != count:
        raise ValueError("fixture-count-invalid")
    return codec, config, struct.unpack_from(">HH", payload, 24), packets


def envelope(codec, config, geometry, packets):
    flags = 2 if codec == b"hev1" else 0
    header = bytes([1 | flags]) + struct.pack("<H", len(config)) + config
    if codec == b"vp09":
        header += struct.pack("<HH", *geometry)
    result = header + b"".join(
        bytes([int(sync)]) + struct.pack("<H", len(packet)) + packet
        for sync, packet in packets
    )
    if len(result) > 65536:
        raise ValueError("seed-size-invalid")
    return result


def seeds():
    result = {}
    for fixture in FIXTURES:
        codec, config, geometry, packets = extract(fixture)
        directory = "hevc" if fixture["codec"] == "hevc" else "vp9"
        name = fixture["id"]
        result[f"{directory}/{name}-config"] = bytes([2 if codec == b"hev1" else 0]) + config
        selected = packets[:16]
        if directory == "hevc":
            next_sync = next(
                index for index, (sync, _) in enumerate(packets[1:], 1) if sync
            )
            selected = packets[:8] + packets[next_sync:next_sync + 8]
        result[f"{directory}/{name}-sequence"] = envelope(codec, config, geometry, selected)
        if directory == "vp9":
            packet = packets[0][1]
            # A one-frame superframe index exercises its explicit tail layout.
            marker = 0xc8
            indexed = (
                packet + bytes([marker])
                + struct.pack("<H", len(packet)) + bytes([marker])
            )
            result[f"{directory}/{name}-superframe"] = envelope(codec, config, geometry, [(True, indexed)])
    for directory in ("hevc", "vp9"):
        result[f"{directory}/empty-configuration"] = b"\0"
        result[f"{directory}/truncated-envelope"] = b"\1\xff\xff"
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    check = parser.parse_args().check
    corpus = ROOT / "fuzz/corpus"
    expected = seeds()
    for name, data in expected.items():
        path = corpus / name
        if check:
            if path.read_bytes() != data:
                raise ValueError("seed-bytes-changed")
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
    actual = {
        path.relative_to(corpus).as_posix()
        for path in corpus.rglob("*") if path.is_file()
    }
    if actual != set(expected):
        raise ValueError("seed-file-set-invalid")
    manifest = {
        "schema_version": 1,
        "source_fixtures": [
            {"path": item["path"], "sha256": item["sha256"]} for item in FIXTURES
        ],
        "seeds": [
            {
                "path": name,
                "bytes": len(data),
                "sha256": hashlib.sha256(data).hexdigest(),
                "accepted": name.rsplit("/", 1)[1] not in (
                    "empty-configuration", "truncated-envelope"
                ),
                "samples": (
                    16 if name.endswith("-sequence")
                    else 1 if name.endswith("-superframe") else 0
                ),
            }
            for name, data in sorted(expected.items())
        ],
    }
    encoded = (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode("ascii")
    path = ROOT / "fuzz/seed-manifest.json"
    if check:
        if path.read_bytes() != encoded:
            raise ValueError("seed-manifest-changed")
    else:
        path.write_bytes(encoded)
    print(f"Verified {len(expected)} public codec seeds")


if __name__ == "__main__":
    main()
