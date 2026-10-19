# Pages browser checks

These test-only files run in GitHub Actions. They are not uploaded as part of the
website, which contains no production JavaScript. No media, keys or conversion
jobs are submitted by these checks.

`npm ci --ignore-scripts` installs the exact lockfile. The workflow audits it,
then uses the official Playwright command to install its pinned Chromium and
system dependencies. Node 24.22.0 is pinned by the workflow. Browser installation
and execution are confined to CI; pure controls use `npm run test:controls`.

The single-worker suite has thirteen tests and zero whole-test retries. Element-state
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
  hosted fonts and images, exact served asset bytes, all 30 native Tab targets and
  anchor navigation, rendered ARIA landmarks/headings/link names, and axe checks
  including color contrast. Both violations and incomplete checks must be empty.
  The five named code groups retain their full command text in both rendered
  accessibility trees. Chromium CDP records actual glyph-producing platform
  fonts for the heading, lead and code, including Japanese fallback; it does
  not establish manual readability or glyph coverage for other text.
- Core content and native anchor navigation with JavaScript disabled in each
  language (two cases). The
  loopback job checks both `/` and the project `/drmr/` mount; the published job
  checks only `/drmr/`.
- A native-keyboard language roundtrip visits English → Japanese → English at
  both loopback mounts, or at the published project mount. Each navigation must
  load the matching raw page hash and declared language without scripts,
  language detection or cookies.
- Negative controls inject overflow, an invisible focus outline and a broken
  image into the isolated browser DOM. Each real measurement must reject its
  corresponding fault; the production files are unchanged.
- An English native Tab comparison replays both observed edge targets in fresh documents
  with the production 16px scroll margin, a test-only zero-margin override, then
  the restored production style. Both production states must satisfy the exact
  complete-focus condition. The zero-margin state must retain partial clipping
  of the correct target throughout the existing five-second observation budget.
  Only the owned condition-timeout type is accepted; evaluation, transport,
  polling and evidence-write errors remain fatal. Offscreen or horizontally
  clipped targets cannot stand in for a healthy partial-bottom control.
  No programmatic scroll, refocus or viewport epsilon is used.

The pre-publish job serves only the nine allowlisted files on loopback. It must
pass, along with static validation on both operating systems, before deployment.
The post-publish job is main-only, needs successful deployment, and has only
`contents: read`. It has one fixed remote destination:
`https://kazuhide.github.io/drmr/`. It does not accept another remote URL.

Before published browser checks, `wait-published.mjs` polls all nine raw file
hashes against the workflow checkout, with at most 30 passes, a 120-second
budget and at most five seconds per request. This bounded propagation wait does
not restart failed browser tests. Redirects are rejected. A later deployment
replacing these bytes can therefore fail an earlier job instead of being
misattributed to its source commit.

Canonical English and Japanese pages are `/drmr/` and `/drmr/ja/`. The static
validator requires their exact language, canonical URL, reciprocal alternates
and visible language links. Only the Japanese document can use one literal
`../` to reach the allowlisted shared assets or English root; traversal beyond
that set is refused. Both pages must directly link to the shared font notice.

Separate loopback and published artifacts retain full-page screenshots, rendered
ARIA snapshots, axe results (including incomplete checks), browser observations,
HTTP asset headers, raw asset SHA-256 values, source commit/tree, per-case locale and raw page hash, dependency and
browser pins, and machine-readable test results for three days. The HTML test
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
redistributed in the nine-file Pages artifact. The npm audit gate is a current
advisory check, not a claim that these dependencies can never have a defect.
The pure Node suite has 16 controls, separate from the 13 CI browser cases and
19 workflow checks (17 successful checks plus intentionally skipped publish/live
jobs on a pull request). Production HTML still contains zero scripts; CI-only
axe injection does not change that source contract.
The hosted IBM Plex font files and their OFL notice remain part of the website.

Official references:

- <https://playwright.dev/docs/ci>
- <https://playwright.dev/docs/accessibility-testing>
- <https://playwright.dev/docs/aria-snapshots>
- <https://github.com/microsoft/playwright/releases/tag/v1.64.1>

- <https://playwright.dev/docs/api/class-browsercontext#browser-context-new-cdp-session>
- <https://chromedevtools.github.io/devtools-protocol/tot/CSS/#method-getPlatformFontsForNode>
