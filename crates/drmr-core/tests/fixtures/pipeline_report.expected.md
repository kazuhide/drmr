# Repackaging report: report-asset

schema_version: 1  
full_enumeration: True  
round_trip_verified: True  
all_pass: False

## v0 (video)

- segments: source=3 output=3
- requested duration: 6.0s
- `6-1.boundary-agreement`: FAIL: segment 0 boundary diverges (source sample_count=281, output sample_count=282)
- `6-1.iv-agreement`: FAIL: no constant IV found in output init segment
- `6-1.number-origin`: PASS
- `6-1.pssh-agreement`: SKIP: verified during manifest validation
- `6-1.segment-duration`: PASS
- `6-4.framemd5`: SKIP: gated on 6-1 duration/number-origin/boundary-agreement PASS

## a0 日本語 (audio)

- segments: source=2 output=0
- requested duration: 5.333333333333333s
- `6-1.boundary-agreement`: SKIP: upstream step failed
- `6-1.iv-agreement`: SKIP: upstream step failed
- `6-1.number-origin`: SKIP: upstream step failed
- `6-1.pssh-agreement`: SKIP: upstream step failed
- `6-1.segment-duration`: SKIP: upstream step failed
- `6-4.framemd5`: SKIP: upstream step failed
- `source.decrypt`: FAIL: exit 5

## v1 (video)

- segments: source=1 output=1
- requested duration: 1e+19s
- `6-1.segment-duration`: PASS
- `6-4.framemd5`: FAIL: "quoted" \ back	slash

## manifests

- dash: `out/report-asset/manifests/manifest.example.test/t/dash/manifest.mpd`
- hls master: `out/report-asset/manifests/manifest.example.test/t/hls/manifest.m3u8`
- `6-1.dash-playready-sync`: PASS
- `6-1.dash-scheme-cbcs`: PASS
- `6-1.dash-segmenttemplate-unchanged`: PASS
- `6-1.dash-widevine-unchanged`: PASS
- `6-1.hls-content-gate`: SKIP: TS H.264 decryption is not supported; segment count, duration, and key signaling checked separately
- `6-1.hls-extinf`: PASS
- `6-1.hls-iv-agreement`: PASS
- `6-1.hls-segment-count`: PASS
- `6-1.hls-version-unchanged`: PASS
- `6-1.pssh-agreement`: PASS
- `6-2.audio-consolidation-structure`: PASS
- `6-2.audio-pcm-identity`: SKIP: run audio_pcm_consolidation_gate with key material
- `6-5.dash-out-of-scope-untouched`: PASS
- `6-5.hls-master-untouched`: PASS
- `6-6.dash-conformance`: SKIP: DASH-IF validator not yet integrated
