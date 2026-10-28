# Terminal progress regressions

These checks run the actual CLI against the attributed ten-second
[Big Buck Bunny fixture](../../crates/drmr/tests/fixtures/bbb/README.md).
FFmpeg makes five two-second HLS transport segments. A test wrapper delays one
selected processing invocation for six seconds and then executes the installed
Shaka Packager unchanged. Version probes run immediately. Success and `NO_COLOR`
use two workers and delay shared source preparation. Cancellation and failure
use one worker and delay the encrypted CENC output, after both output parent
slots exist. Failure returns a tool error after the same six-second delay.

A saved bundle plan produces encrypted DASH and clear HLS outputs using stitch
processing. This shared-preparation route supplies the pipeline estimates and
rendition bars checked by the regression.

```sh
npm ci --ignore-scripts
npm audit --audit-level=low
npm run test:controls
DRMR_CONSOLE_BINARY=/path/to/drmr TERMINAL_REPORT_ROOT=/temporary/evidence npm run test:terminal
```

The PTY begins at 120 columns, changes to 80, 40 and 300 columns, and returns to
120 columns immediately after reading part of a new dashboard header. This
places a resize inside an old-width paint without depending on the scheduler to
interrupt an application write. The output side has automatic newline conversion
disabled, so the CLI must emit the carriage returns it needs.

The pinned official `@xterm/headless` engine receives every recorded byte in its
original read order. Each write callback completes before the next write or
resize. Complete paints require the two output parents in order, their known
formats once processing starts, and the matching source state. They also verify
one header, ASCII bars and their starting column. The entire primary-screen
transcript must contain the preserved history and the matching footer. For the
controlled cancellation/failure phase, exactly three stopped-summary rows follow:
Source checked, DASH stopped, then the second output not started. Indented,
duplicated, reordered or live rows are rejected, including residue erased later.

Four cases cover success with shared TTY input, success with `NO_COLOR` and an
unread input pipe, cancellation, and a real tool failure. Queued input and terminal
input settings must remain unchanged. Successful outputs undergo independent
packet, timing, decoded-media and native-manifest verification. Cancellation and
failure must return structured JSON and leave no published output. A real PTY
metadata analysis with `TERM=linux` separately requires plain progress and a
valid result without alternate-screen entry or an owned screen clear.

FFmpeg, FFprobe and Packager calls, including fixture preparation, planning and
verification, use registered proxies and bounded command execution. Cleanup
checks kernel birth identities and uses PID-bound signaling on the supported
Linux/macOS host. Unknown identity or unconfirmed termination fails the check
and retains the owned temporary directory. An actual timeout control launches
all three tools in separate process groups and requires CLI/tool termination
and released locks; separate controls cover reused PIDs and uncertain identity.

CI reuses the console job's actual debug CLI build, Node 24 and installed media
tools. Its report contains hashes, fixed assertion names, numeric frame data and
byte/resize offsets. Raw terminal bytes, command JSON, tool arguments, temporary
paths and key configuration stay within the test process and its owned temporary
directory. Confirmed cleanup removes the owned processes and media on success
and failure; unconfirmed cleanup retains its evidence and fails the run.
