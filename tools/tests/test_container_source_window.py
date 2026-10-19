"""The fixture window is copied exactly; malformed or ambiguous metadata is refused."""

import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "container_source_window",
    Path(__file__).resolve().parents[1] / "container_source_window.py",
)
module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(module)


def box(kind, payload=b""):
    return struct.pack(">I4s", len(payload) + 8, kind) + payload


def full_header(scale, duration=0, version=0):
    return (
        bytes([version, 0, 0, 0])
        + bytes(16 if version else 8)
        + struct.pack(">I", scale)
        + duration.to_bytes(8 if version else 4, "big")
    )


def tkhd(duration=0, version=0):
    return (
        bytes([version, 0, 0, 7])
        + bytes(16 if version else 8)
        + struct.pack(">II", 1, 0)
        + duration.to_bytes(8 if version else 4, "big")
    )


def edit(duration, start, version=0, rate=1, entries=1):
    width = 8 if version else 4
    return box(
        b"edts",
        box(
            b"elst",
            bytes([version, 0, 0, 0])
            + struct.pack(">I", entries)
            + duration.to_bytes(width, "big")
            + start.to_bytes(width, "big", signed=True)
            + struct.pack(">hh", rate, 0),
        ),
    )


def movie(
    fragmented=False,
    movie_scale=1000,
    media_scale=48000,
    duration=1000,
    start=0,
    version=0,
    rate=1,
    entries=1,
    offset=0,
):
    window = b"" if fragmented else edit(duration, start, version, rate, entries)
    media_duration = 0 if fragmented else duration * media_scale // movie_scale + start
    stco = box(
        b"stco",
        bytes(4)
        + struct.pack(">I", 1 if offset else 0)
        + (struct.pack(">I", offset) if offset else b""),
    )
    mdia = box(
        b"mdia",
        box(b"mdhd", full_header(media_scale, media_duration, version))
        + box(b"hdlr", bytes(8) + b"vide")
        + box(b"minf", box(b"stbl", stco)),
    )
    track = box(
        b"trak",
        box(b"tkhd", tkhd(0 if fragmented else duration, version)) + window + mdia,
    )
    moov = box(
        b"moov",
        box(b"mvhd", full_header(movie_scale, 0 if fragmented else duration, version))
        + track
        + (box(b"mvex", box(b"trex", bytes(24))) if fragmented else b""),
    )
    return box(b"ftyp", b"mp41" + bytes(4) + b"iso8isomcmfc") + moov


def boxes(data):
    result = []
    while data:
        size, kind = struct.unpack(">I4s", data[:8])
        result.append((kind, data[8:size]))
        data = data[size:]
    return result


def one(data, kind):
    return [payload for name, payload in boxes(data) if name == kind][0]


class WindowTests(unittest.TestCase):
    def exercise(self, source, init):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            src = root / "source.mp4"
            dst = root / "init.mp4"
            src.write_bytes(source)
            dst.write_bytes(init)
            try:
                module.preserve_video_window(src, dst)
            except ValueError:
                self.assertEqual(src.read_bytes(), source)
                self.assertEqual(dst.read_bytes(), init)
                raise
            self.assertEqual(src.read_bytes(), source)
            return dst.read_bytes()

    def test_exact_movie_and_media_scale_conversion_preserves_headers_and_other_bytes(
        self,
    ):
        source = movie(duration=1000, start=24000)
        init = movie(True, movie_scale=90000, media_scale=96000)
        actual = self.exercise(source, init)
        old = one(one(init, b"moov"), b"trak")
        new = one(one(actual, b"moov"), b"trak")
        self.assertEqual(one(old, b"tkhd"), one(new, b"tkhd"))
        self.assertEqual(one(old, b"mdia"), one(new, b"mdia"))
        self.assertEqual(
            one(one(init, b"moov"), b"mvhd"), one(one(actual, b"moov"), b"mvhd")
        )
        elst = one(one(new, b"edts"), b"elst")
        self.assertEqual(
            elst, bytes([1, 0, 0, 0]) + struct.pack(">IQQhh", 1, 90000, 48000, 1, 0)
        )
        self.assertEqual(one(actual, b"ftyp"), b"mp41" + bytes(4) + b"iso8isom")

    def test_version_one_source_window_is_preserved(self):
        source = movie(version=1, duration=2000, start=48000)
        actual = self.exercise(source, movie(True, version=1))
        elst = one(one(one(one(actual, b"moov"), b"trak"), b"edts"), b"elst")
        self.assertEqual(
            elst, bytes([1, 0, 0, 0]) + struct.pack(">IQQhh", 1, 2000, 48000, 1, 0)
        )

    def test_malformed_or_ambiguous_source_window_is_refused_without_writing(self):
        for source in [
            movie(rate=0),
            movie(start=-1),
            movie(entries=2),
            movie(duration=0),
            movie()[:-1],
            movie(True),
        ]:
            with self.subTest(source=source[:16]):
                with self.assertRaisesRegex(
                    ValueError, "^container-source-window-invalid$"
                ):
                    self.exercise(source, movie(True))

    def test_nonintegral_movie_or_media_scale_conversion_is_refused(self):
        for source, init in [
            (movie(movie_scale=3, duration=1), movie(True, movie_scale=1000)),
            (movie(media_scale=3, start=1), movie(True, media_scale=1000)),
        ]:
            with (
                self.subTest(source=source[:16]),
                self.assertRaisesRegex(ValueError, "^container-source-window-invalid$"),
            ):
                self.exercise(source, init)

    def test_initialized_media_existing_edits_and_absolute_chunk_offsets_are_refused(
        self,
    ):
        init = movie(True)
        moov = one(init, b"moov")
        track = one(moov, b"trak")
        # Rebuild the parent size rather than creating a merely truncated test input.
        edited = box(b"ftyp", one(init, b"ftyp")) + box(
            b"moov",
            moov.replace(box(b"trak", track), box(b"trak", track + edit(1000, 0))),
        )
        for candidate in [
            movie(),
            init + box(b"mdat", b"media"),
            movie(True, offset=8),
            edited,
        ]:
            with (
                self.subTest(candidate=candidate[:16]),
                self.assertRaisesRegex(ValueError, "^container-source-window-invalid$"),
            ):
                self.exercise(movie(), candidate)

    def test_duplicate_tracks_are_not_silently_selected(self):
        init = movie(True)
        moov = one(init, b"moov")
        track = box(b"trak", one(moov, b"trak"))
        altered = box(b"ftyp", one(init, b"ftyp")) + box(b"moov", moov + track)
        with self.assertRaisesRegex(ValueError, "^container-source-window-invalid$"):
            self.exercise(movie(), altered)

    def test_malformed_box_does_not_replace_initialization(self):
        for source in [
            b"\0" * 8,
            struct.pack(">I4s", 2, b"moov"),
            struct.pack(">I4s", 0xFFFFFFFF, b"moov"),
        ]:
            with (
                self.subTest(source=source),
                self.assertRaisesRegex(ValueError, "^container-source-window-invalid$"),
            ):
                self.exercise(source, movie(True))

    def test_size_bound_refuses_before_any_initialization_write(self):
        with (
            patch.object(module, "MAX_BYTES", 32),
            self.assertRaisesRegex(ValueError, "^container-source-window-invalid$"),
        ):
            self.exercise(movie(), movie(True))

    def test_nonzero_fragmented_header_duration_is_not_silently_repaired(self):
        init = movie(True)
        moov = one(init, b"moov")
        mvhd = box(b"mvhd", one(moov, b"mvhd"))
        altered = box(b"ftyp", one(init, b"ftyp")) + box(
            b"moov", moov.replace(mvhd, box(b"mvhd", full_header(1000, 1)))
        )
        with self.assertRaisesRegex(ValueError, "^container-source-window-invalid$"):
            self.exercise(movie(), altered)

    def test_cmaf_major_and_compatible_claims_are_removed_without_adding_brands(self):
        init = movie(True)
        payload = b"cmfc" + bytes(4) + b"iso8isomcmfc"
        init = box(b"ftyp", payload) + box(b"moov", one(init, b"moov"))
        actual = self.exercise(movie(), init)
        self.assertEqual(one(actual, b"ftyp"), b"isom" + bytes(4) + b"iso8isom")
