# Pages browser checks

These test-only files run in GitHub Actions. They are not uploaded as part of the
website, which contains no production JavaScript. The landing and guide checks
submit no media, keys or conversion jobs.

`npm ci --ignore-scripts` installs the exact lockfile. The workflow audits it,
then uses the official Playwright command to install its pinned Chromium and
system dependencies. Node 24.22.0 is pinned by the workflow. Browser installation
and execution are confined to CI; pure controls use `npm run test:controls`.

Two single-worker suites have nineteen landing tests and twelve guide tests.
Each retains a separate 180-second total budget and zero whole-test retries. Element-state
assertions can wait for at most five seconds. After each native Tab, read-only
observations require the same target, complete viewport containment and the
existing focus outline. They never scroll or refocus to make a check pass.
Production focusable elements have 16px of vertical scroll margin so native
focus leaves room for the 8px outline outset and fractional layout edges. The
complete viewport comparisons retain zero epsilon.
One absolute five-second deadline bounds awaiting every measurement and delegated
polling wait. An unfinished operation at expiry is a fatal observation error,
not an accepted negative control. During the final owned cadence window no
operation is outstanding; healthy partial observations can then expire as a
condition timeout. Late completions cannot append samples or pass.
Every observation records element identity, the full rectangle, viewport, scroll
position and time; failed checks retain these samples too. Persistent clipping
still fails. The suite covers:

- English and Japanese at 320, 768, 1024 and 1440 px (eight cases): document overflow and clipped layout boxes, locally
  hosted fonts and images, exact served asset bytes, all 81 English / 80 Japanese native Tab targets and
  anchor navigation, rendered ARIA landmarks/headings/link names, and axe checks
  including color contrast. Both violations and incomplete checks must be empty.
  The five named code groups retain their full command text in both rendered
  accessibility trees. The first main section is a five-input-profile by four-output-format
  semantic table. Exact row/column headers, all 20 output cells, input protection
  qualifications and linked notes are checked independently of the HTML.
  All 45 supported output-protection tokens link to their exact guide recipe;
  unavailable cells have no recipe links. Focused matrix links also undergo
  point hit-testing so sticky input headers cannot cover their rendered boxes.
  Output links have atomic layout boxes. Only keyboard-focused links define a
  horizontal snap position; scrollport padding keeps them beside the sticky
  input label. Pointer clicks retain the cell layout, including when switching
  from keyboard navigation after returning from a guide example.
  Each scrollable matrix is reached by native Tab and traversed to both
  horizontal edges with arrow keys. A listener is armed before each key, and
  its native `scrollend` must arrive before geometry is accepted or another key
  is sent; partial animation deltas cannot complete a key. The entire traversal
  retains its five-second deadline. Sticky row headers and document scroll
  positions remain unchanged. Its first and last output columns must fit next
  to the input headers. Only the matrix owns intentional horizontal overflow;
  page overflow and clipped cells remain failures. Nonempty rendered text must
  retain visible leaf/ancestor styles and actual rectangles inside its cell;
  hidden child labels cannot pass through unchanged DOM text. Initial viewport screenshots
  are captured after navigation/font readiness and before keyboard/focus/scroll
  interaction, alongside the existing full-page captures. Chromium CDP records actual glyph-producing platform
  fonts for the heading, lead and code, including Japanese fallback; it does
  not establish manual readability or glyph coverage for other text.
- Core content and native anchor navigation with JavaScript disabled in each
  language (two cases). The
  loopback job checks both `/` and the project `/drmr/` mount; the published job
  checks only `/drmr/`.
- Six separate cases activate all 45 matrix links in each language at the project
  mount with JavaScript disabled. Each case covers fifteen distinct recipes,
  retaining the same thirty-second case budget. Native Tab/Enter activates the
  first recipe of each batch; the remaining links use actual link clicks. Each destination
  must expose its exact focusable heading, setup link and complete four-step
  command example with matching output format, protection and saved plan.
  The served guide hash must match the workflow source. A 320px viewport image
  records each batch's first native recipe destination before further guide interaction,
  with unchanged heading geometry and complete source metadata.
- A native-keyboard language roundtrip visits English → Japanese → English at
  both loopback mounts, or at the published project mount. Each navigation must
  load the matching raw page hash and declared language without scripts,
  language detection or cookies.
- Negative controls inject overflow, an invisible focus outline and a broken
  image, hide real Clear labels and block native matrix scrolling in the isolated browser DOM. Each real measurement must reject its
  corresponding fault. Removing the matrix override must restore actual native
  arrow movement; the production files are unchanged.
- An English native Tab comparison first discovers a naturally partial-bottom
  target using only native Tab with zero scroll margin. Only correct active
  identity, positive dimensions, the intact 3px outline and partial bottom
  clipping can select a candidate. No candidate, entirely offscreen content,
  horizontal clipping and transport/evaluation errors remain failures.
  The same width, ordinal and complete identity are replayed in fresh documents
  with production 16px margin, zero margin, then restored production style.
  Production states must satisfy complete focus visibility. The zero-margin
  state must retain healthy partial clipping throughout the existing absolute
  five-second observation budget. Only the owned condition-timeout type passes
  this negative control. Discovery routes and all replay samples are retained.
  No programmatic scroll, refocus, viewport epsilon or whole-test retry is used.

The pre-publish landing and guide jobs serve only twelve allowlisted files on
loopback. Both must pass, along with static validation on both operating systems,
before deployment. The guide suite separately covers:

- Both languages at 320, 768, 1024 and 1440 px: all twenty ordered chapters,
  full accessible code examples and labelled tables, local fonts and brand
  images, zero document overflow, and empty axe violations and incomplete checks.
  Each width retains three 900px-high viewport images: the untouched initial view,
  the widest native code owner after its full roundtrip, and the widest table
  owner after its full roundtrip. The latter two record the same focused owner,
  exact source/page identity and unchanged geometry/document position before and
  after capture (`viewport-native-v1`). Each capture remains bounded to five
  seconds. These are visual checkpoints, not full-guide pixel coverage; all
  twenty chapters and complete code/table content retain automated checks.
  Independent figure assertions run in batches of sixteen. Each figure's DOM
  attributes and rendered text use one read-only measurement, while the full
  figure and code ARIA assertions remain separate. Phase timings identify
  content, accessibility-tree, native-interaction and axe costs without changing
  the case or suite deadlines.
- Native Tab/Enter reaches the contents and selected chapter, then the widest
  code block and table. Each overflowing target is pinned to one ElementHandle,
  traversed to both edges and returned to zero using the existing shared absolute
  five-second key-completion mechanism. Focus, target identity and document
  position remain exact; no manual scroll or focus makes these checks pass.
- Both guide languages work with production JavaScript disabled. Native language
  links retain the selected chapter in both directions, at both local mounts or
  the one published mount.
- Chromium print-media styles retain all chapter content, code and tables while
  hiding duplicated navigation. This checks computed print state, not physical
  printer output or paginated PDF appearance. An injected hidden chapter heading
  is rejected by the actual guide measurement and restored before further checks.

Code examples use native named figures and captions, without overriding the
figure role. Validated code-language metadata permits visual JSON wrapping while
retaining complete original code text; shell continuations retain command tokens.
Static validation requires exact regeneration of both committed guide pages
from the standard-library builder, recipe generator, template and two Markdown sources. The
payload remains bounded to 640 KiB, including all twelve files; Markdown,
templates, build code and test dependencies are outside the uploaded site.
The source commit/tree, twelve payload hashes and five builder/input hashes
are retained in both browser suites. Generated output is checked again from
that checkout before browser or published-byte inspection. No generator or
JavaScript runs in the production page.
The post-publish job is main-only, needs successful deployment, and has only
`contents: read`. It has one fixed remote destination:
`https://kazuhide.github.io/drmr/`. It does not accept another remote URL.

Before published browser checks, `wait-published.mjs` polls all twelve raw file
hashes against the workflow checkout, with at most 30 passes, a 120-second
budget and at most five seconds per request. This bounded propagation wait does
not restart failed browser tests. Redirects are rejected. A later deployment
replacing these bytes can therefore fail an earlier job instead of being
misattributed to its source commit.

Canonical English and Japanese pages are `/drmr/` and `/drmr/ja/`. The static
validator requires their exact language, canonical URL, reciprocal alternates
and visible language links. The nested guides are `/drmr/guide/` and `/drmr/ja/guide/`, with their own
reciprocal canonical/alternate pair and same-chapter language links. Only leading
literal parents inside the originating document depth can reach an explicitly
allowlisted page or shared asset; encoded parents, internal detours and escapes
remain refused. All four pages directly link to the shared font notice.

Separate loopback and published artifacts retain landing full-page screenshots,
guide viewport checkpoints, rendered
ARIA snapshots, axe results (including incomplete checks), browser observations,
HTTP asset headers, raw asset SHA-256 values, source commit/tree, per-case locale and raw page hash, dependency and
browser pins, and machine-readable test results for three days. Landing and
guide reports use separate `landing/` and `guide/` evidence directories. There
are six Pages jobs: pull requests require four successes and two intentional
publish/live skips; main requires all six successes. The main-only live job runs
both bounded suites after publication. The HTML test
report is evidence, not a production website asset. Missing or stale published
bytes and browser failures remain failures.

These checks demonstrate automated Chromium behavior, not manual NVDA/VoiceOver
operation, complete WCAG conformance, other browsers or devices. External GitHub
document links are checked against repository paths by the static validator;
the browser never navigates to them. CSS boxes are checked horizontally and
keyboard focus is checked within the viewport; screenshots remain available for
human visual review. Layout measurements are not a substitute for that review.

## Test dependencies

The lockfile has five exact development packages: `@playwright/test`,
`playwright` and `playwright-core` 1.64.1 (Apache-2.0), plus
`@axe-core/playwright` and `axe-core` 4.14.0 (MPL-2.0). They are CI-only and are not
redistributed in the twelve-file Pages artifact. The npm audit gate is a current
advisory check, not a claim that these dependencies can never have a defect.
The pure Node controls are separate from the 19 landing and 12 guide browser cases and
the applicable workflow checks, which depend on changed paths. Publish/live
jobs are intentionally skipped on pull requests. Production HTML still contains zero scripts; CI-only
axe injection does not change that source contract.
The hosted IBM Plex font files and their OFL notice remain part of the website.

## Common console checks

The separate `test:console` suite builds and serves the actual CLI and embedded
Web/Desktop console. It has 34 cases, one worker, a ten-minute total budget and
zero whole-test retries. It uses a short Big Buck Bunny fixture and synthetic
keys in runner-owned private directories. Tests cover local input and authenticated
HLS acquisition, saved plans shared with the CLI, multiple encrypted outputs,
fresh independent verification, failed checks, interrupted requests and stopping
each operation. The suite also checks English/Japanese forms, keyboard use,
layout and accessibility at ten widths.

Evidence binds the source tree, CLI build, served assets, jobs and displayed
results. The lifecycle gate requires authenticated fixture requests, confirmed
server exit and removal of the owned temporary directory. These checks qualify
Linux Chromium and real media tools; native WebView interaction, other browsers,
device playback and DRM license services require separate qualification.

Official references:

- <https://playwright.dev/docs/ci>
- <https://playwright.dev/docs/accessibility-testing>
- <https://playwright.dev/docs/aria-snapshots>
- <https://github.com/microsoft/playwright/releases/tag/v1.64.1>

- <https://playwright.dev/docs/api/class-browsercontext#browser-context-new-cdp-session>
- <https://chromedevtools.github.io/devtools-protocol/tot/CSS/#method-getPlatformFontsForNode>
