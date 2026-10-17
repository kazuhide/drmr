#!/usr/bin/env python3
"""Fake Shaka Packager for unit tests -- NEVER a real binary, produces NO real media.

The Packager wrapper is exercised here against a synthetic subprocess stub. The
stub records the argv it was invoked with and fabricates empty placeholder outputs so
the wrapper's file handling does not choke. Behaviour is driven entirely by env vars
so a single stub covers every test mode:

  STUB_PACKAGER_ARGV_FILE   -- if set, the invocation argv (one token per line) is
                               written here for the test to inspect. Overwritten each
                               call, so it captures only the LAST invocation.
  STUB_PACKAGER_ARGV_DIR    -- if set, EACH invocation's argv is written to its own
                               zero-padded, monotonically-numbered file (0000.txt,
                               0001.txt, ...) in this dir, so a multi-subprocess caller
                               (e.g. the per-segment transcryption loop) can inspect every
                               call in order. Backward-compatible with ARGV_FILE.
  STUB_PACKAGER_STDERR      -- "echo_argv" makes the stub leak its full command line
                               (key + iv included) to stderr, to prove the Packager wrapper
                               scrubs it; any other non-empty value is emitted verbatim.
  STUB_PACKAGER_EXIT        -- process exit code (default "0").
  STUB_PACKAGER_HANG        -- "1" makes the stub sleep, so the wrapper's timeout path
                               (subprocess.TimeoutExpired) can be exercised.
  STUB_PACKAGER_SEGMENT_COUNT -- how many 1-origin $Number$ segments to fabricate
                               for encrypt-mode staging templates. If UNSET, the count is
                               derived from --segment_duration to model real Shaka (see
                               _segment_count): an isolating duration (>> content, used by
                               the per-segment transcryption path) yields exactly ONE
                               segment, a normal duration yields the default 3. An explicit
                               value always wins, for tests that pin an exact count.
"""
import os
import pathlib
import sys
import time

# A --segment_duration at least this large (seconds) models "longer than the whole
# content" -> real Shaka emits a single segment. The per-segment transcryption path uses
# an isolating duration of 1e6 s; any real segment duration is far below.
_ISOLATING_DURATION_THRESHOLD_S = 10_000.0
_DEFAULT_SEGMENT_COUNT = 3


def _flag_value(argv, name):
    for i, tok in enumerate(argv):
        if tok == name and i + 1 < len(argv):
            return argv[i + 1]
    return None


def _segment_count(argv):
    """How many 1-origin $Number$ segments to fabricate. An explicit
    STUB_PACKAGER_SEGMENT_COUNT always wins; otherwise model real Shaka's behaviour where a
    --segment_duration longer than the content produces exactly one segment (the isolating
    duration the per-segment transcryption relies on), and a normal duration produces the
    default count."""
    explicit = os.environ.get("STUB_PACKAGER_SEGMENT_COUNT")
    if explicit is not None:
        return int(explicit)
    duration = _flag_value(argv, "--segment_duration")
    if duration is not None:
        try:
            if float(duration) >= _ISOLATING_DURATION_THRESHOLD_S:
                return 1
        except ValueError:
            pass
    return _DEFAULT_SEGMENT_COUNT


def _create_outputs(argv):
    """Fabricate the output files declared in the Shaka stream descriptor.

    The stream descriptor is the argv token that carries both '=' and ',' (e.g.
    'input=a.mp4,stream=0,output=b.mp4'); global flags and their values never carry
    both, so this cleanly ignores '--keys label=...:key=...' and friends.
    """
    for token in argv:
        if "=" not in token or "," not in token:
            continue
        fields = dict(kv.split("=", 1) for kv in token.split(",") if "=" in kv)
        for key in ("output", "init_segment"):
            if key in fields:
                out = pathlib.Path(fields[key])
                out.parent.mkdir(parents=True, exist_ok=True)
                out.write_bytes(b"")
        if "segment_template" in fields:
            template = fields["segment_template"]
            count = _segment_count(argv)
            for number in range(1, count + 1):  # Shaka's $Number$ is 1-origin
                seg = pathlib.Path(template.replace("$Number$", str(number)))
                seg.parent.mkdir(parents=True, exist_ok=True)
                seg.write_bytes(b"")


def main():
    argv = sys.argv[1:]

    argv_file = os.environ.get("STUB_PACKAGER_ARGV_FILE")
    if argv_file:
        pathlib.Path(argv_file).write_text("\n".join(argv), encoding="utf-8")

    argv_dir = os.environ.get("STUB_PACKAGER_ARGV_DIR")
    if argv_dir:
        d = pathlib.Path(argv_dir)
        d.mkdir(parents=True, exist_ok=True)
        # Monotonic index by counting existing capture files -- tests drive the stub
        # sequentially, so this preserves call order (decrypt/encrypt interleaving).
        index = len(list(d.glob("[0-9]" * 4 + ".txt")))
        (d / f"{index:04d}.txt").write_text("\n".join(argv), encoding="utf-8")

    stderr_mode = os.environ.get("STUB_PACKAGER_STDERR", "")
    if stderr_mode == "echo_argv":
        sys.stderr.write("shaka packager error; args were: " + " ".join(argv) + "\n")
    elif stderr_mode:
        sys.stderr.write(stderr_mode + "\n")

    _create_outputs(argv)

    if os.environ.get("STUB_PACKAGER_HANG") == "1":
        time.sleep(60)

    sys.exit(int(os.environ.get("STUB_PACKAGER_EXIT", "0")))


if __name__ == "__main__":
    main()
