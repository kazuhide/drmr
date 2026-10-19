"""Copy one explicit video edit into a qualification-only Shaka initialization segment.

This bounded fixture helper is not a general MP4 editor. Sample data and fragments
are never rewritten; all original media comparisons remain the caller's authority.
"""

from contextlib import contextmanager
import os
import stat
import struct

BAD = "container-source-window-invalid"
MAX_BYTES = 32 * 1024 * 1024


def refuse():
    raise ValueError(BAD)


def boxes(data):
    result = []
    at = 0
    while at < len(data):
        if len(result) >= 10000 or len(data) - at < 8:
            refuse()
        size, kind = struct.unpack_from(">I4s", data, at)
        # Qualification fixtures use complete 32-bit boxes, never EOF or wide sizes.
        if size < 8 or size > len(data) - at:
            refuse()
        result.append((kind, data[at + 8 : at + size]))
        at += size
    return result


def one(data, kind):
    selected = [payload for name, payload in boxes(data) if name == kind]
    if len(selected) != 1:
        refuse()
    return selected[0]


def boxed(kind, payload):
    if len(payload) + 8 > MAX_BYTES:
        refuse()
    return struct.pack(">I4s", len(payload) + 8, kind) + payload


def replace(data, kind, replacement):
    one(data, kind)
    return b"".join(
        boxed(name, replacement if name == kind else payload)
        for name, payload in boxes(data)
    )


def integer(data, at, width=4, signed=False):
    if at + width > len(data):
        refuse()
    return int.from_bytes(data[at : at + width], "big", signed=signed)


def header(data):
    if not data or data[0] not in (0, 1):
        refuse()
    width = 8 if data[0] else 4
    scale = integer(data, 20 if data[0] else 12)
    if not scale:
        refuse()
    return scale, integer(data, 24 if data[0] else 16, width)


def track_duration(data):
    if not data or data[0] not in (0, 1):
        refuse()
    return integer(data, 28 if data[0] else 20, 8 if data[0] else 4)


def video(data):
    moov = one(data, b"moov")
    track = one(moov, b"trak")
    mdia = one(track, b"mdia")
    if one(mdia, b"hdlr")[8:12] != b"vide":
        refuse()
    return moov, track, mdia


def source_window(data):
    moov, track, mdia = video(data)
    if any(kind == b"mvex" for kind, _ in boxes(moov)):
        refuse()
    movie_scale, _ = header(one(moov, b"mvhd"))
    media_scale, _ = header(one(mdia, b"mdhd"))
    edit = one(one(track, b"edts"), b"elst")
    if not edit or edit[0] not in (0, 1):
        refuse()
    width = 8 if edit[0] else 4
    if (
        edit[1:4] != bytes(3)
        or integer(edit, 4) != 1
        or len(edit) != 12 + 2 * width
        or edit[-4:] != b"\x00\x01\x00\x00"
    ):
        refuse()
    duration = integer(edit, 8, width)
    start = integer(edit, 8 + width, width, signed=True)
    if not duration or start < 0:
        refuse()
    return duration, movie_scale, start, media_scale


def rescale(value, source_scale, target_scale):
    result, remainder = divmod(value * target_scale, source_scale)
    if remainder or result > (1 << 63) - 1:
        refuse()
    return result


def empty_offsets(data, depth=0):
    if depth > 8:
        refuse()
    for kind, payload in boxes(data):
        if kind in (b"stco", b"co64") and payload != bytes(8):
            refuse()
        if kind in (b"saio", b"iloc"):
            refuse()
        if kind in (b"moov", b"trak", b"mdia", b"minf", b"stbl", b"mvex"):
            empty_offsets(payload, depth + 1)


def initialization(source, data):
    if len(source) > MAX_BYTES or len(data) > MAX_BYTES:
        refuse()
    if {kind for kind, _ in boxes(data)} != {b"ftyp", b"moov"}:
        refuse()
    duration, source_movie_scale, start, source_media_scale = source_window(source)
    moov, track, mdia = video(data)
    one(moov, b"mvex")
    if any(kind == b"edts" for kind, _ in boxes(track)):
        refuse()
    movie_scale, movie_duration = header(one(moov, b"mvhd"))
    media_scale, media_duration = header(one(mdia, b"mdhd"))
    if movie_duration or media_duration or track_duration(one(track, b"tkhd")):
        refuse()
    empty_offsets(data)
    duration = rescale(duration, source_movie_scale, movie_scale)
    start = rescale(start, source_media_scale, media_scale)
    # elst v1 stores a 64-bit movie duration and signed 64-bit media start.
    edit = bytes([1, 0, 0, 0]) + struct.pack(">IQQhh", 1, duration, start, 1, 0)
    track += boxed(b"edts", boxed(b"elst", edit))
    data = replace(data, b"moov", replace(moov, b"trak", track))
    ftyp = one(data, b"ftyp")
    if len(ftyp) < 8 or len(ftyp) % 4:
        refuse()
    major = b"isom" if ftyp[:4].startswith(b"cmf") else ftyp[:4]
    # Keep existing ISO compatibility; changing edits does not qualify CMAF brands.
    brands = b"".join(
        ftyp[i : i + 4]
        for i in range(8, len(ftyp), 4)
        if not ftyp[i : i + 4].startswith(b"cmf")
    )
    return replace(data, b"ftyp", major + ftyp[4:8] + brands)


@contextmanager
def regular(path, writable=False):
    before = path.lstat()
    if (
        not stat.S_ISREG(before.st_mode)
        or before.st_nlink != 1
        or getattr(before, "st_file_attributes", 0)
        & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
        or before.st_size > MAX_BYTES
    ):
        refuse()
    fd = os.open(
        path, (os.O_RDWR if writable else os.O_RDONLY) | getattr(os, "O_NOFOLLOW", 0)
    )
    with os.fdopen(fd, "r+b" if writable else "rb") as stream:
        actual = os.fstat(stream.fileno())
        if (actual.st_dev, actual.st_ino, actual.st_size) != (
            before.st_dev,
            before.st_ino,
            before.st_size,
        ):
            refuse()
        yield stream


def preserve_video_window(source, initialization_path):
    """Change only an owned, media-free init after all bounded checks succeed."""
    try:
        with regular(source) as original, regular(initialization_path, True) as target:
            source_id = os.fstat(original.fileno())
            target_id = os.fstat(target.fileno())
            if (source_id.st_dev, source_id.st_ino) == (
                target_id.st_dev,
                target_id.st_ino,
            ):
                refuse()
            output = initialization(
                original.read(MAX_BYTES + 1), target.read(MAX_BYTES + 1)
            )
            target.seek(0)
            target.write(output)
            target.truncate()
    except (OSError, ValueError, struct.error, OverflowError) as error:
        raise ValueError(BAD) from error
