# Terminal progress regressions

These checks run the actual CLI against the attributed ten-second
[Big Buck Bunny fixture](../../crates/drmr/tests/fixtures/bbb/README.md).
FFmpeg makes five two-second HLS transport segments. A test wrapper delays the
first processing invocation for six seconds and then executes the installed
Shaka Packager unchanged. Version probes run immediately. The failure case
instead returns a tool error after the same delay.

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
resize. Observations at synchronized-output transaction boundaries verify one
header, visible parent rows, ASCII progress bars and their starting column.
Final checks require the normal screen, the original history line, a visible
cursor, a completion summary and no dashboard rows.

Four cases cover success with shared TTY input, success with `NO_COLOR` and an
unread input pipe, cancellation, and a real tool failure. Queued input and terminal
input settings must remain unchanged. Successful outputs undergo independent
packet, timing, decoded-media and native-manifest verification. Cancellation and
failure must return structured JSON and leave no published output.

CI reuses the console job's actual debug CLI build, Node 24 and installed media
tools. Its report contains hashes, fixed assertion names, numeric frame data and
byte/resize offsets. Raw terminal bytes, command JSON, tool arguments, temporary
paths and key configuration stay within the test process and its owned temporary
directory. Test processes and media are cleaned up on success and failure.
