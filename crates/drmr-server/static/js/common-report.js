/* Typed common-result evidence. Missing, unrequested and incomplete checks stay distinct. */
(function () {
  'use strict';
  var App = window.App;
  function checksPassed(checks) {
    return !!checks && checks.stream_metadata === 'pass' && checks.packet_payloads_equal === true && checks.packet_timing_equal === true && checks.decoded_content_equal === true;
  }
  function nativeChecksPassed(checks) {
    return !!checks && checks.stream_metadata === 'pass' && checks.packet_payloads_equal === true && checks.packet_timing_equal === true;
  }
  function decodeSatisfied(report) {
    var evidence = report && report.decode_evidence;
    if (!evidence || ['native', 'decoded'].indexOf(report.required_depth) === -1 || ['source-admission', 'comparison'].indexOf(report.scope) === -1) return false;
    if (evidence.status === 'not-checked') return evidence.reason === 'not-requested' && report.required_depth === 'native';
    if (evidence.status === 'source-decoded') return report.scope === 'source-admission';
    return evidence.status === 'content-compared' && report.scope === 'comparison' && evidence.equal === true;
  }
  function nativeState(report) {
    if (!report) return 'not-checked';
    if (Number.isSafeInteger(report.errors) && report.errors > 0) return 'fail';
    if (!Number.isSafeInteger(report.errors) || !Number.isSafeInteger(report.warnings) || report.errors < 0 || report.warnings < 0 || report.truncated !== false || report.incomplete !== false) return 'incomplete';
    return report.warnings > 0 ? 'warning' : 'pass';
  }
  function verificationPassed(report) {
    if (report && report.schema_version === 2) return report.passed === true && report.failure == null && report.license_playback_verified === false && nativeChecksPassed(report.checks) && decodeSatisfied(report) && (!report.native_manifest || nativeState(report.native_manifest) === 'pass' || nativeState(report.native_manifest) === 'warning');
    return !!report && report.schema_version === 1 && report.passed === true && report.media_verified === true && report.license_playback_verified === false && checksPassed(report.checks) && (!report.native_manifest || nativeState(report.native_manifest) === 'pass' || nativeState(report.native_manifest) === 'warning');
  }
  function signalingState(report) {
    if (!report || !Array.isArray(report.systems)) return 'incomplete';
    if (!report.systems.length) return 'not-requested';
    return report.verified === true ? 'pass' : 'fail';
  }
  function conformanceState(report) {
    if (report === null) return 'not-requested';
    if (!report || !Array.isArray(report.checks) || !report.checks.length) return 'incomplete';
    return report.passed === true && report.checks.every(function (check) { return check.passed === true; }) ? 'pass' : 'fail';
  }
  function outputPassed(output, verify, bundle, version) {
    var conversion = output && (output.conversion || output);
    return !!output && (!bundle || Object.prototype.hasOwnProperty.call(output, 'signaling') && Object.prototype.hasOwnProperty.call(output, 'conformance')) && (!verify || output.output_matches_plan === true) && !!conversion.verification && conversion.verification.schema_version === version && (version !== 2 || conversion.verification.scope === 'comparison') && verificationPassed(conversion.verification) && (!Object.prototype.hasOwnProperty.call(output, 'signaling') || signalingState(output.signaling) === 'pass' || signalingState(output.signaling) === 'not-requested') && (!Object.prototype.hasOwnProperty.call(output, 'conformance') || conformanceState(output.conformance) === 'pass' || conformanceState(output.conformance) === 'not-requested');
  }
  function evidencePassed(envelope) {
    if (!envelope || !envelope.report) return false;
    var report = envelope.report;
    if (report.error || report.status === 'failed' || report.status === 'cancelled') return false;
    if (envelope.report_kind === 'plan') {
      var legacy = (report.schema_version === 1 || report.schema_version === 2) && !Object.prototype.hasOwnProperty.call(report, 'required_depth');
      var current = [3, 4, 5, 6, 7, 8, 9, 10].indexOf(report.schema_version) !== -1 && ['native', 'decoded'].indexOf(report.required_depth) !== -1;
      return (legacy || current) && typeof report.plan_id === 'string' && /^[a-f0-9]{64}$/.test(report.plan_id);
    }
    if (report.schema_version !== 1 && report.schema_version !== 2) return false;
    if (envelope.report_kind === 'analysis') return report.input_admitted === true && (report.schema_version === 2 ? !!report.verification && report.verification.scope === 'source-admission' && verificationPassed(report.verification) : checksPassed(report.input_checks));
    if (envelope.report_kind === 'conversion') {
      return Array.isArray(report.outputs) ? report.outputs.length > 0 && report.outputs.length <= 8 && report.outputs.every(function (output) { return outputPassed(output, false, true, report.schema_version); }) : outputPassed(report, false, false, report.schema_version);
    }
    if (envelope.report_kind === 'verification') {
      if (report.source_unchanged !== true) return false;
      return Array.isArray(report.outputs) ? report.passed === true && report.outputs.length > 0 && report.outputs.length <= 8 && report.outputs.every(function (output) { return outputPassed(output, true, true, report.schema_version); }) : outputPassed(report, true, false, report.schema_version);
    }
    return false;
  }
  var ERROR_KEYS = {
    "source-layout-path-unsafe": "commonUnsupported",
    "source-layout-collision": "commonUnsupported",
    "source-layout-not-supported": "commonUnsupported",
    "output-options-invalid": "commonPlanInvalid",
    "execution-plan-output-options-changed": "commonPlanInvalid",
    "dash-addressing-requires-dash": "commonInvalid",
    "dash-addressing-source-not-supported": "commonUnsupported",
    "dash-nominal-duration-not-representable": "commonDashTimelineRequired",
    "dash-nominal-time-template-not-supported": "commonUnsupported",
    "dash-indexed-absolute-fragment-offset-not-supported": "commonUnsupported",
    "dash-index-sap-not-supported": "commonUnsupported",
    "dash-index-timeline-not-representable": "commonUnsupported",
    "dash-index-not-representable": "commonUnsupported",
    "dash-index-timescale-not-supported": "commonUnsupported",
    "dash-presentation-origin-timescale-not-supported": "commonUnsupported",
    "dash-indexed-output-collision": "commonUnsupported",
    "invalid-avc-syntax": "commonVerificationFailed",
    "native-aac-clock-mismatch": "commonVerificationFailed",
    "native-aac-config-invalid": "commonVerificationFailed",
    "native-aac-config-not-supported": "commonUnsupported",
    "native-avc-access-unit-size-limit": "commonResourceBudget",
    "native-avc-sync-sample-mismatch": "commonVerificationFailed",
    "native-cipher-counter-overflow": "commonVerificationFailed",
    "native-codec-configuration-not-supported": "commonUnsupported",
    "native-codec-profile-not-supported": "commonUnsupported",
    "native-media-clock-overflow": "commonUnsupported",
    "native-media-duration-mismatch": "commonVerificationFailed",
    "native-mp4-invalid": "commonVerificationFailed",
    "native-mp4-profile-not-supported": "commonUnsupported",
    "native-presentation-metadata-invalid": "commonVerificationFailed",
    "native-presentation-window-not-supported": "commonUnsupported",
    "native-sample-evidence-incomplete": "commonVerificationFailed",
    "native-sample-evidence-invalid": "commonVerificationFailed",
    "native-sample-protection-mismatch": "commonVerificationFailed",
    "native-source-changed": "commonSourceChanged",
    "native-track-changed": "commonSourceChanged",
    "native-video-dimensions-mismatch": "commonVerificationFailed",
    "presentation-window-outside-media": "commonVerificationFailed",
    "ts-aac-mp4-invalid": "commonVerificationFailed",
    "ts-aac-mp4-write-failed": "commonError",
    "ts-avc-picture-duration-not-supported": "commonUnsupported",
    "ts-avc-separate-colour-plane-not-supported": "commonUnsupported",
    "ts-avc-slice-groups-not-supported": "commonUnsupported",
    "ts-avc-slice-order-not-supported": "commonUnsupported",
    "ts-segment-evidence-incomplete": "commonVerificationFailed",
    "ts-segment-evidence-limit": "commonResourceBudget",
    "ts-segment-evidence-missing": "commonVerificationFailed",
    "ts-segment-evidence-unfinished": "commonVerificationFailed",
    "ts-video-duration-clock-required": "commonVerificationFailed",
    "ts-video-reorder-limit": "commonResourceBudget",
    "verification-native-evidence-required": "commonVerificationFailed",
    "request-pending": "commonPending",
    "verification-mismatch": "commonVerificationFailed",
    "request-limit-reached": "commonCapacityFull",
    "request-outcome-unknown": "commonUnknownOwner",
    "request-timeout": "commonUnknownOwner",
    "response-binding-invalid": "commonUnknownOwner",
    "cleanup-unconfirmed": "commonCleanup",
    "server-changed": "commonServerChanged",
    "global-job-busy": "commonGlobalBusy",
    "session-busy": "commonGlobalBusy",
    "server-shutting-down": "commonGlobalClosing",
    "private-config-unavailable": "commonPrivateConfig",
    "selection-not-permitted": "commonPrivateConfig",
    "destination-exists": "commonDestinationExists",
    "artifact-unavailable": "commonArtifactUnavailable",
    "invalid-request": "commonInvalid",
    "worker-limit-exceeded": "commonInvalid",
    "planned-source-changed": "commonSourceChanged",
    "execution-plan-source-changed": "commonSourceChanged",
    "download-source-changed": "commonSourceChanged",
    "analysis-source-changed": "commonSourceChanged",
    "key-file-invalid": "commonKeyFileInvalid",
    "output-spec-invalid": "commonOutputSpecInvalid",
    "input-key-required": "commonInputKeysRequired",
    "hls-input-key-required": "commonInputKeysRequired",
    "output-key-required": "commonOutputKeysRequired",
    "output-key-role-mismatch": "commonOutputKeysMismatch",
    "output-key-id-mismatch": "commonOutputKeysMismatch",
    "execution-plan-output-keys-mismatch": "commonOutputKeysMismatch",
    "execution-plan-drm-mismatch": "commonDrmMismatch",
    "invalid-encrypted-segment": "commonDecryptionFailed",
    "source-decryption-verification-failed": "commonDecryptionFailed",
    "invalid-input-key": "commonDecryptionFailed",
    "manifest-validator-unavailable": "commonValidatorUnavailable",
    "manifest-conformance-failed": "commonConformanceFailed",
    "media-tool-unavailable-or-timeout": "commonMediaToolUnavailable",
    "media-tool-failed": "commonMediaToolFailed",
    "round-trip-verification-failed": "commonVerificationFailed",
    "source-media-verification-failed": "commonVerificationFailed",
    "source-decode-comparison-failed": "commonVerificationFailed",
    "verification-evidence-scope-mismatch": "commonVerificationFailed",
    "output-signaling-verification-failed": "commonDrmMismatch",
    "source-normalization-verification-failed": "commonVerificationFailed",
    "execution-plan-invalid": "commonPlanInvalid",
    "execution-plan-unsupported": "commonUnsupported",
    "conversion-not-supported": "commonUnsupported",
    "fragment-timing-not-supported": "commonUnsupported",
    "unsupported-profile": "commonUnsupported",
    "download-source-not-supported": "commonUnsupported",
    "download-source-unavailable": "commonFetchFailed",
    "fetch-policy-invalid": "commonFetchInvalid",
    "output-exists-or-unsafe": "commonDestinationExists",
    "output-exists-or-publish-failed": "commonDestinationExists",
    "resource-budget": "commonResourceBudget",
    "processing-segments-source-required": "commonUnsupported",
    "processing-container-change-not-supported": "commonUnsupported",
    "processing-profile-not-supported": "commonUnsupported",
    "processing-plan-override-not-supported": "commonPlanFrozen",
    "processing-selection-invalid": "commonPlanInvalid",
    "request-too-large": "commonResourceBudget",
    "cancelled": "commonInterrupted"
};
  function errorInfo(error) {
    var code = error && error.code;
    return typeof code === 'string' && Object.prototype.hasOwnProperty.call(ERROR_KEYS, code) ? { code: code, key: ERROR_KEYS[code] } : { code: null, key: 'commonError' };
  }
  // Every transition is a public schema name. Output map IDs use only the '*' token.
  var CONFIGURATION_NODES = {
    keys: { expected: 'keys-object', fields: { schema_version: 'schema-version', input: 'input', output: 'output-key', outputs: 'output-domains' } },
    outputs: { expected: 'outputs-object', fields: { schema_version: 'schema-version', outputs: 'outputs-array' } },
    input: { expected: 'input-object', fields: { keys: 'input-keys', hls_keys: 'hls-keys' } },
    'input-keys': { expected: 'input-keys', item: 'key-object' },
    'key-object': { expected: 'key-object', fields: { kid: 'hex-16-bytes', key: 'hex-16-bytes' } },
    'hls-keys': { expected: 'hls-keys', item: 'hls-key-object' },
    'hls-key-object': { expected: 'hls-key-object', fields: { uri: 'uri', key: 'hex-16-bytes', iv: 'segment-iv' } },
    'output-domains': { expected: 'output-domains', fields: { '*': 'output-key' } },
    'output-key': { expected: 'output-key', fields: { kid: 'hex-16-bytes', key: 'hex-16-bytes', hls_key_uri: 'uri', iv: 'segment-iv', signaling: 'signaling' } },
    signaling: { expected: 'signaling', fields: { system: 'signaling-system', license_url: 'license-url', widevine_pssh: 'pssh', playready_license_url: 'license-url', fairplay: 'boolean' } },
    'outputs-array': { expected: 'outputs-array', item: 'output-object' },
    'output-object': { expected: 'output-object', fields: { id: 'output-id', format: 'output-format', target: 'target-protection', drms: 'drms', conformance: 'conformance', processing: 'processing', dash_addressing: 'dash-addressing' } },
    drms: { expected: 'drms', item: 'drm-system' }
  };
  function configurationInfo(error) {
    var detail = error && error.configuration, own = Object.prototype.hasOwnProperty;
    if (!detail || typeof detail !== 'object' || Array.isArray(detail) || detail.schema_version !== 1 ||
      !((error.code === 'key-file-invalid' && detail.file === 'keys') || (error.code === 'output-spec-invalid' && detail.file === 'outputs')) ||
      Object.keys(detail).some(function (key) { return ['schema_version', 'file', 'path', 'expected', 'line', 'column'].indexOf(key) === -1; }) ||
      !Array.isArray(detail.path) || detail.path.length > 16) return null;
    var node = detail.file, location = '';
    for (var index = 0; index < detail.path.length; index++) {
      var step = detail.path[index], state = own.call(CONFIGURATION_NODES, node) ? CONFIGURATION_NODES[node] : null;
      if (!state) return null;
      if (Number.isSafeInteger(step) && step >= 0 && step <= 65536 && state.item) {
        node = state.item; location += '[' + step + ']';
      } else if (typeof step === 'string' && state.fields && own.call(state.fields, step)) {
        node = state.fields[step]; location += (location ? '.' : '') + step;
      } else return null;
    }
    var expected = own.call(CONFIGURATION_NODES, node) ? CONFIGURATION_NODES[node].expected : node;
    if (detail.expected !== expected) return null;
    var hasPosition = own.call(detail, 'line') || own.call(detail, 'column');
    if (hasPosition && !(Number.isSafeInteger(detail.line) && detail.line >= 1 && detail.line <= 65537 &&
      Number.isSafeInteger(detail.column) && detail.column >= 0 && detail.column <= 65536)) return null;
    return { file: detail.file, path: location || 'root', expected: expected,
      position: hasPosition ? { line: detail.line, column: detail.column } : null };
  }
  function render(host, envelope) {
    var UI = App.UI, h = UI.h, updates = [];
    var metadata = envelope && envelope.report ? envelope : {};
    var token = /^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$/;
    [
      ['job-id', metadata.job_id, token], ['request-id', metadata.request_id, token],
      ['operation', metadata.operation, /^(analyze|plan|convert|verify)$/],
      ['report-kind', metadata.report_kind, /^(analysis|plan|conversion|verification|failure)$/],
      ['plan-id', metadata.plan_id, /^[a-f0-9]{64}$/], ['plan-ref', metadata.plan_ref, token]
    ].forEach(function (field) {
      var attribute = 'data-common-' + field[0], value = field[1];
      if (typeof value === 'string' && field[2].test(value)) host.setAttribute(attribute, value); else host.removeAttribute(attribute);
    });
    function t(key, values) { return App.I18n.t(key, values); }
    function word(tag, key, props) { return h(tag, Object.assign({ text: t(key), dataset: { i18n: key } }, props || {})); }
    function dynamic(tag, key, values, props) {
      var read = function () { return typeof values === 'function' ? values() : values; };
      var node = h(tag, Object.assign({ text: t(key, read()) }, props || {}));
      updates.push(function () { UI.setText(node, t(key, read())); }); return node;
    }
    function badge(state) {
      var keys = { pass: 'commonPass', fail: 'commonFail', incomplete: 'commonIncomplete', warning: 'commonWarning', 'not-requested': 'commonNotRequested', 'not-checked': 'commonNotChecked', unknown: 'commonUnknown' };
      return word('span', keys[state] || 'commonUnknown', { className: 'chip chip-' + (state === 'pass' ? 'good' : state === 'fail' ? 'fail' : state === 'warning' || state === 'incomplete' ? 'warn' : 'info') });
    }
    function flag(value) { return badge(value === true ? 'pass' : value === false ? 'fail' : 'unknown'); }
    function checkState(name, state) { return { 'data-common-check': name, 'data-common-state': state }; }
    function check(key, value, name) {
      var state = value === true ? 'pass' : value === false ? 'fail' : 'unknown';
      return h('li', checkState(name, state), word('span', key), badge(state));
    }
    function code(value) { return h('code', { text: typeof value === 'string' && /^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$/.test(value) ? value : t('commonUnknown') }); }
    function table(headers, rows) {
      return h('div', { className: 'table-wrap', tabIndex: 0 }, h('table', { className: 'data' },
        h('thead', null, h('tr', null, headers.map(function (key) { return word('th', key, { scope: 'col' }); }))),
        h('tbody', null, rows)));
    }
    function inspection(input, qualified) {
      if (!input) return null;
      var fields = h('dl', { className: 'kv' },
        word('dt', 'commonFormat'), h('dd', null, code(input.format)),
        word('dt', 'commonProtection'), h('dd', null, code(input.protection)),
        word('dt', 'commonEvidence'), h('dd', null, code(input.evidence)));
      var tracks = Array.isArray(input.tracks) ? input.tracks : [];
      return h('div', { className: 'common-result-block' }, fields,
        omitted(tracks, 512), table(['commonTrackNumber', 'commonTrackKind', 'commonCodec', 'commonDeclaredProtection', 'commonQualifiedProtection'], tracks.slice(0, 512).map(function (track, index) {
          return h('tr', null, h('td', { text: String(index + 1) }), h('td', null, code(track.kind)), h('td', null, code(track.codec)), h('td', null, code(track.protection)), h('td', null, code(qualified && qualified[index])));
        })));
    }
    function omitted(rows, max) {
      return rows.length > max ? dynamic('p', 'commonRowsOmitted', { shown: max, total: rows.length }, { className: 'notice notice-warn' }) : null;
    }
    function reason(value) {
      var keys = { 'missing-linked-manifest': 'commonReasonLinked', 'unsupported-feature': 'commonReasonUnsupported', 'requires-media-evidence': 'commonReasonMedia', 'requires-delivery-evidence': 'commonReasonDelivery', 'resource-limit': 'commonReasonLimit', 'invalid-prerequisite': 'commonReasonPrerequisite', 'missing-reference-base': 'commonReasonBase' };
      return word('span', Object.prototype.hasOwnProperty.call(keys, value) ? keys[value] : 'commonUnknown');
    }
    function documentNumber(row) {
      var value = row.document_ordinal == null ? row.document : row.document_ordinal;
      return Number.isSafeInteger(value) && value >= 0 ? String(value) : t('commonUnknown');
    }
    function native(report) {
      var state = nativeState(report);
      var result = h('div', { className: 'common-result-block' }, h('div', Object.assign({ className: 'evidence' }, checkState('native_manifest', state)), word('span', 'commonNativeManifest'), badge(state)));
      if (!report) return result;
      result.appendChild(dynamic('p', 'commonNativeCounts', { errors: report.errors, warnings: report.warnings, documents: report.document_count }, { className: 'help' }));
      result.appendChild(word('p', 'commonManifestScope', { className: 'help' }));
      result.appendChild(h('dl', { className: 'kv' },
        word('dt', 'commonProfile'), h('dd', null, code(report.profile)),
        word('dt', 'commonProfileRevision'), h('dd', null, code(report.profile_revision)),
        word('dt', 'commonEngineRevision'), h('dd', null, code(report.engine_revision)),
        word('dt', 'commonTruncated'), h('dd', null, word('span', report.truncated === true ? 'commonYes' : report.truncated === false ? 'commonNo' : 'commonUnknown')),
        word('dt', 'commonCompleteness'), h('dd', null, word('span', report.incomplete === true ? 'commonYes' : report.incomplete === false ? 'commonNo' : 'commonUnknown'))));
      var details = h('details', { className: 'disclosure' }, word('summary', 'commonCoverage'));
      var diagnostics = Array.isArray(report.diagnostics) ? report.diagnostics : [];
      var body = h('div', { className: 'disclosure-body common-result-block' });
      if (diagnostics.length) body.appendChild(table(['commonDocument', 'commonRule', 'commonStatus'], diagnostics.slice(0, 1000).map(function (row) {
        var location = row.location;
        var document = location ? documentNumber(location) : t('commonUnknown');
        return h('tr', null, h('td', { text: document }), h('td', null, code(row.rule_id)), h('td', null, badge(row.severity === 'error' ? 'fail' : row.severity === 'warning' ? 'warning' : 'unknown')));
      })));
      var diagnosticOmission = omitted(diagnostics, 1000); if (diagnosticOmission) body.appendChild(diagnosticOmission);
      var coverage = Array.isArray(report.coverage) ? report.coverage : [];
      if (coverage.length) body.appendChild(table(['commonDocument', 'commonRule', 'commonScope', 'commonStatus', 'commonReason'], coverage.slice(0, 1000).map(function (row) {
        var status = row.status || {}, state = { passed: 'pass', failed: 'fail', 'not-checked': 'not-checked' }[status.status] || 'unknown';
        var reasons = Array.isArray(row.not_checked_reasons) ? row.not_checked_reasons : status.reason ? [status.reason] : [];
        return h('tr', null,
          h('td', { text: documentNumber(row) }), h('td', null, code(row.rule_id)),
          h('td', null, word('span', row.scope === 'in-scope' ? 'commonInScope' : row.scope === 'outside-scope' ? 'commonOutsideScope' : 'commonUnknown')),
          h('td', null, badge(state), dynamic('p', 'commonCoverageCounts', { passed: row.passed_checks, failed: row.failed_checks, notChecked: row.not_checked_checks }, { className: 'help' })),
          h('td', null, reasons.map(reason)));
      })));
      var coverageOmission = omitted(coverage, 1000); if (coverageOmission) body.appendChild(coverageOmission);
      details.appendChild(body); result.appendChild(details); return result;
    }
    function verify(report) {
      var checks = report && report.checks || {};
      var license = report && report.license_playback_verified === false ? 'not-checked' : 'unknown';
      if (report && report.schema_version === 2) {
        var evidence = report.decode_evidence || {}, state = evidence.status === 'not-checked' ? 'not-checked' : decodeSatisfied(report) ? 'pass' : 'fail';
        var decodedKey = evidence.status === 'source-decoded' ? 'commonSourceDecoded' : 'commonDecoded';
        return h('div', { className: 'common-result-block' },
          h('dl', { className: 'kv' }, word('dt', 'commonRequiredCoverage'), h('dd', null, word('span', report.required_depth === 'decoded' ? 'commonDecodedCoverage' : 'commonNativeCoverage'))),
          h('ul', { className: 'checks' },
            check('commonRequestedCoverage', report.passed, 'required_coverage'),
            check('commonMetadata', checks.stream_metadata === 'pass' ? true : checks.stream_metadata === 'fail' ? false : null, 'stream_metadata'),
            check('commonPayload', checks.packet_payloads_equal, 'packet_payloads_equal'),
            check('commonTiming', checks.packet_timing_equal, 'packet_timing_equal'),
            h('li', checkState('decode_evidence', state), word('span', decodedKey), badge(state)),
            h('li', checkState('license_playback_verified', license), word('span', 'commonLicensePlayback'), badge(license))),
          native(report.native_manifest));
      }
      return h('div', { className: 'common-result-block' },
        h('ul', { className: 'checks' },
          check('commonMediaVerified', report && report.media_verified, 'media_verified'),
          check('commonMetadata', checks.stream_metadata === 'pass' ? true : checks.stream_metadata === 'fail' ? false : null, 'stream_metadata'),
          check('commonPayload', checks.packet_payloads_equal, 'packet_payloads_equal'),
          check('commonTiming', checks.packet_timing_equal, 'packet_timing_equal'),
          check('commonDecoded', checks.decoded_content_equal, 'decoded_content_equal'),
          h('li', checkState('license_playback_verified', license), word('span', 'commonLicensePlayback'), badge(license))),
        native(report && report.native_manifest));
    }
    function output(row, index, independent) {
      var conversion = row.conversion || row, verification = conversion.verification;
      var section = h('section', { className: 'common-result-block', 'data-common-output-index': String(index) }, row.id == null ? word('h3', 'commonSingleOutput') : h('h3', null, code(row.id)));
      if (typeof row.id === 'string' && /^[a-z0-9][a-z0-9_-]{0,31}$/.test(row.id)) section.setAttribute('data-common-output-id', row.id);
      if (independent) section.appendChild(h('ul', { className: 'checks' }, check('commonMatchesPlan', row.output_matches_plan, 'output_matches_plan')));
      section.appendChild(verify(verification));
      if (Object.prototype.hasOwnProperty.call(row, 'signaling')) {
        var signaling = signalingState(row.signaling);
        section.appendChild(h('div', Object.assign({ className: 'evidence' }, checkState('signaling', signaling)), word('span', 'commonDrms'), badge(signaling), (row.signaling.systems || []).map(code)));
      }
      if (Object.prototype.hasOwnProperty.call(row, 'conformance')) {
        var conformance = row.conformance;
        var conformanceStatus = conformanceState(conformance);
        section.appendChild(h('div', Object.assign({ className: 'evidence' }, checkState('conformance', conformanceStatus)), word('span', 'commonConformance'), badge(conformanceStatus)));
        if (conformance && Array.isArray(conformance.checks)) section.appendChild(table(['commonRule', 'commonStatus', 'commonCheckedManifests', 'commonWarnings', 'commonVersion'], conformance.checks.map(function (check) {
          return h('tr', null, h('td', null, code(check.tool)), h('td', null, flag(check.passed)), h('td', { text: Number.isSafeInteger(check.checked_manifest_count) ? String(check.checked_manifest_count) : t('commonUnknown') }), h('td', { text: Number.isSafeInteger(check.warning_count) ? String(check.warning_count) : t('commonUnknown') }), h('td', null, check.observed_version ? code(check.observed_version) : word('span', 'commonNotChecked')));
        })));
      }
      if (conversion.processing) {
        var processing = conversion.processing;
        var routes = ['progressive-mp4', 'fragmented-mp4', 'dash-presentation', 'fmp4-hls-presentation', 'mpeg-ts-hls', 'mpeg-ts-dash', 'mpeg-ts-sample-aes-hls', 'mpeg-ts-hls-ts', 'fmp4-hls-ts', 'fmp4-segments', 'mpeg-ts-segments'];
        var details = h('dl', { className: 'kv' }, word('dt', 'commonProcessing'), h('dd', null, code(routes.indexOf(processing.route) !== -1 ? processing.route : null)));
        var mode = ['segments', 'stitch'].indexOf(processing.mode) !== -1 && { segments: 'commonProcessingSegments', stitch: 'commonProcessingStitch' }[processing.mode];
        var boundaries = ['preserve', 'resegment', 'not-applicable'].indexOf(processing.boundaries) !== -1 && { preserve: 'commonBoundaryPreserve', resegment: 'commonBoundaryResegment', 'not-applicable': 'commonBoundaryNotApplicable' }[processing.boundaries];
        if (mode) { details.setAttribute('data-common-processing-mode', processing.mode); details.appendChild(word('dt', 'commonProcessingMode')); details.appendChild(word('dd', mode)); }
        if (boundaries) { details.setAttribute('data-common-processing-boundaries', processing.boundaries); details.appendChild(word('dt', 'commonBoundaries')); details.appendChild(word('dd', boundaries)); }
        section.appendChild(details);
      }
      return section;
    }
    function configurationView(error) {
      var detail = configurationInfo(error);
      if (!detail) return null;
      var view = h('dl', { className: 'kv', 'data-common-configuration': detail.file },
        word('dt', 'commonConfigurationFile'), h('dd', null, code(detail.file)),
        word('dt', 'commonConfigurationField'), h('dd', null, h('code', { text: detail.path })),
        word('dt', 'commonConfigurationExpected'), word('dd', 'commonConfigurationExpected_' + detail.expected));
      if (detail.position) {
        view.appendChild(word('dt', 'commonConfigurationPosition'));
        view.appendChild(dynamic('dd', 'commonConfigurationLineColumn', detail.position));
      }
      return view;
    }
    function failureView(value) {
      var info = errorInfo(value.error);
      var operation = { analyze: 'commonAnalyze', plan: 'commonSavePlan', convert: 'commonConvert', verify: 'commonVerify' }[envelope.operation] || 'commonUnknown';
      var activity = value.error && value.error.last_activity;
      var activities = ['configuration', 'acquisition', 'input-preparation', 'inspection', 'normalization', 'packaging', 'verification', 'publication'];
      return h('div', { className: 'common-result-block' },
        dynamic('p', 'commonFailedOperation', function () { return { operation: t(operation) }; }, { className: 'help' }),
        h('div', { className: 'notice notice-' + (value.status === 'cancelled' ? 'warn' : 'fail'), role: value.status === 'cancelled' ? 'status' : 'alert' }, word('span', info.key), info.code ? code(info.code) : null),
        configurationView(value.error),
        activities.indexOf(activity) !== -1 ? h('p', { className: 'help' }, word('span', 'commonLastActivity'), word('span', 'commonActivity_' + activity)) : null);
    }
    var report = envelope && envelope.report, content = [];
    if (!report) content.push(word('p', 'commonNoResult', { className: 'help' }));
    else if (envelope.report_kind === 'analysis') {
      content.push(h('ul', { className: 'checks' }, check('commonInputAdmitted', report.input_admitted, 'input_admitted')), inspection(report.input, report.track_protection),
        h('dl', { className: 'kv' }, word('dt', 'commonActualFormat'), h('dd', null, code(report.actual_format)), word('dt', 'commonEligible'), h('dd', null, (report.eligible_formats || []).map(function (format, index) { return [index ? ', ' : null, code(format)]; }))));
      if (Array.isArray(report.compatibility_notes) && report.compatibility_notes.length) content.push(dynamic('p', 'commonInspectionNotes', { n: report.compatibility_notes.length }, { className: 'help' }));
      if (report.schema_version === 2 && report.verification) content.push(verify(report.verification));
    } else if (envelope.report_kind === 'conversion' || envelope.report_kind === 'verification') {
      if (envelope.report_kind === 'verification') content.push(h('ul', { className: 'checks' }, check('commonSourceUnchanged', report.source_unchanged, 'source_unchanged')));
      var rows = Array.isArray(report.outputs) ? report.outputs : [report];
      content.push(omitted(rows, 8));
      rows.slice(0, 8).forEach(function (row, index) { content.push(output(row, index, envelope.report_kind === 'verification')); });
      content.push(word('p', 'commonVerifyHelp', { className: 'help' }));
    } else if (envelope.report_kind === 'failure') content.push(failureView(report));
    else content.push(word('p', 'commonError', { className: 'help' }));
    if (report && envelope.report_kind !== 'failure' && report.error) content.push(failureView(report));
    UI.replace(host, content);
    return function () { updates.forEach(function (update) { update(); }); };
  }
  App.CommonReport = { checksPassed: checksPassed, verificationPassed: verificationPassed, nativeState: nativeState, signalingState: signalingState, conformanceState: conformanceState, evidencePassed: evidencePassed, errorInfo: errorInfo, render: render };
})();
