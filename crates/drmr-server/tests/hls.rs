mod support;

use base64::Engine as _;
use drmr_core::canonical::Dumps;
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use support::{
    Answer, FAIRPLAY, Harness, HarnessSetup, IV, KEY, KID, LA_URL, b64url, error, sse_events,
};

const ORIGIN: &str = "http://127.0.0.1:9999";
const SOURCE_URL: &str = "http://127.0.0.1:9999/master.m3u8?token=PRIVATE";
const PLAIN_URL: &str = "http://127.0.0.1:9999/master.m3u8";
const PLAYLIST: &str = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nsegment.ts\n#EXT-X-ENDLIST\n";
/// An init segment whose `avcC` box proves `avc1.64001e`.
const AVC_INIT: &[u8] = b"\x00\x00\x00\x13avcC\x01\x64\x00\x1e\xff\xe1\x00\x00\x00\x00\x00\x00\x00";

fn widevine_pssh() -> String {
    let mut boxed = Vec::new();
    boxed.extend_from_slice(&32u32.to_be_bytes());
    boxed.extend_from_slice(b"pssh");
    boxed.extend_from_slice(&0u32.to_be_bytes());
    boxed.extend_from_slice(&hex::decode("edef8ba979d64acea3c827dcd51d21ed").unwrap());
    boxed.extend_from_slice(&0u32.to_be_bytes());
    base64::engine::general_purpose::STANDARD.encode(boxed)
}

fn drm() -> Value {
    json!({"kid": KID, "key": KEY, "iv": IV, "widevine_pssh": widevine_pssh(),
           "playready_la_url": LA_URL, "fairplay_uri": FAIRPLAY})
}

// ---- the fixture ------------------------------------------------------------------------------

struct Web {
    h: Harness,
    clock: Arc<AtomicU64>,
    profiles: PathBuf,
}

fn web() -> Web {
    web_with(|_| {})
}

fn web_with(configure: impl FnOnce(&mut HarnessSetup)) -> Web {
    let clock = Arc::new(AtomicU64::new(10f64.to_bits()));
    let reader = clock.clone();
    let mut profiles = PathBuf::new();
    let h = Harness::with(|setup| {
        profiles = setup.base.join("profiles.json");
        let policy = json!({"default": {"policy": {"allowed_origins": [ORIGIN], "allow_http": true, "allow_private_network": true}}});
        std::fs::write(&profiles, policy.to_string()).unwrap();
        support::set_mode(&profiles, 0o600);
        setup.options.hls_profiles = Some(profiles.clone());
        setup.options.monitor = false;
        setup.options.hls_clock = Some(Arc::new(move || {
            f64::from_bits(reader.load(Ordering::SeqCst))
        }));
        configure(setup);
    });
    h.cli.hold();
    Web { h, clock, profiles }
}

impl Web {
    fn get(&self, path: &str) -> Answer {
        self.h.client.get(path)
    }

    fn post(&self, path: &str, body: &Value) -> Answer {
        self.h.client.post_json(path, body)
    }

    fn advance(&self, seconds: f64) {
        let now = f64::from_bits(self.clock.load(Ordering::SeqCst));
        self.clock
            .store((now + seconds).to_bits(), Ordering::SeqCst);
    }

    /// The stub's `finish(job_id, code)`: the held child exits with `code`.
    fn finish(&self, job_id: &str, code: i32) {
        self.h.cli.release(code);
        support::wait_until(|| {
            self.h
                .app()
                .manager
                .status(job_id)
                .map_or(true, |handle| handle.state != "running")
        });
    }

    fn state(&self, job_id: &str) -> Value {
        self.get(&format!("/api/jobs/{job_id}")).json()["state"].clone()
    }

    fn job_ids(&self) -> Vec<String> {
        listed_ids(&self.get("/api/hls/jobs").json())
    }

    /// The argv of the latest launch (once every launch has recorded itself).
    fn last_argv(&self) -> Vec<String> {
        self.h.cli.argv(self.h.cli.calls() - 1)
    }

    /// `service.manager.busy = True`: a catalog conversion holds the shared lease.
    fn occupy_lease(&self) -> String {
        support::stage(&self.h.catalog, "lease-holder");
        let answer = self.post("/api/convert", &json!({"asset_id": "lease-holder"}));
        assert_eq!(answer.status, 202, "{}", answer.text());
        answer.json()["job_id"].as_str().unwrap().to_string()
    }
}

fn listed_ids(page: &Value) -> Vec<String> {
    page["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["job_id"].as_str().unwrap().to_string())
        .collect()
}

// ---- the snapshot the analysis child writes -----------------------------------------------------

/// A file of the frozen source.
#[derive(Clone)]
struct File {
    name: String,
    data: Vec<u8>,
}

fn file(name: &str, data: &[u8]) -> File {
    File {
        name: name.to_string(),
        data: data.to_vec(),
    }
}

fn ts_segment() -> File {
    let mut data = vec![0x47];
    data.extend(std::iter::repeat_n(b'x', 187));
    file("r000001.ts", &data)
}

/// One track as `analyze_hls` records it.
#[derive(Clone)]
struct Track {
    id: String,
    content_type: String,
    container: String,
    codec: String,
    segments: Vec<File>,
    init: Option<File>,
    fields: Map<String, Value>,
}

fn track(id: &str, content_type: &str, container: &str, codec: &str, segments: Vec<File>) -> Track {
    Track {
        id: id.into(),
        content_type: content_type.into(),
        container: container.into(),
        codec: codec.into(),
        segments,
        init: None,
        fields: Map::new(),
    }
}

impl Track {
    fn with(mut self, key: &str, value: Value) -> Track {
        self.fields.insert(key.into(), value);
        self
    }

    fn init(mut self, init: File) -> Track {
        self.init = Some(init);
        self
    }

    /// `_track_private(track, directory)`.
    fn row(&self) -> Value {
        let segments: Vec<Value> = self
            .segments
            .iter()
            .enumerate()
            .map(|(index, segment)| {
                json!({"index": index, "path": segment.name, "duration": 6.0,
                       "sha256": drmr_core::snapshot::sha256_hex(&segment.data), "byte_range": null})
            })
            .collect();
        let count = u32::try_from(segments.len()).unwrap();
        let mut row = json!({
            "id": self.id, "content_type": self.content_type, "container": self.container,
            "codec": self.codec, "codecs": "", "language": "und", "role": "main", "group_id": "",
            "name": "", "width": null, "height": null, "bandwidth": null,
            "segment_count": segments.len(), "duration": 6.0 * f64::from(count),
            "playlist_id": "", "muxed": false, "audio_group": "", "subtitle_group": "",
            "profile": "", "is_default": false, "autoselect": false, "stream_index": 0,
            "init_path": self.init.as_ref().map(|init| init.name.clone()),
            "init_sha256": self.init.as_ref().map(|init| drmr_core::snapshot::sha256_hex(&init.data)),
            "segments": segments,
        });
        for (key, value) in &self.fields {
            row[key] = value.clone();
        }
        row
    }
}

fn video() -> Track {
    track("v000", "video", "ts", "h264", vec![ts_segment()])
        .with("width", json!(640))
        .with("height", json!(360))
        .with("playlist_id", json!("r000000"))
}

/// A frozen snapshot.
struct Snapshot {
    analysis_id: String,
    digest: String,
    path: PathBuf,
    dir: PathBuf,
}

/// What `analyze_hls` leaves under `<output>/<asset>/clear/source/`: the resources, the private
/// record with its digest and id.
fn write_snapshot(output: &Path, asset: &str, tracks: &[Track]) -> Snapshot {
    let dir = output.join(asset).join("clear").join("source");
    support::private_dir(&dir).unwrap();
    let mut resources = Vec::new();
    let mut seen = HashSet::new();
    let mut freeze = |name: &str, data: &[u8], kind: &str| {
        std::fs::write(dir.join(name), data).unwrap();
        if seen.insert(name.to_string()) {
            let stem = name.split('.').next().unwrap();
            resources.push(json!({"id": stem, "path": name, "sha256": drmr_core::snapshot::sha256_hex(data),
                                  "kind": kind, "url": format!("{ORIGIN}/{name}?token=PRIVATE"), "byte_range": null}));
        }
    };
    freeze("r000000.m3u8", PLAYLIST.as_bytes(), "playlist");
    for track in tracks {
        if let Some(init) = &track.init {
            freeze(&init.name, &init.data, "init");
        }
        for segment in &track.segments {
            freeze(&segment.name, &segment.data, "segment");
        }
    }
    let rows: Vec<Value> = tracks.iter().map(Track::row).collect();
    let mut private =
        json!({"schema_version": "1", "asset_id": asset, "resources": resources, "tracks": rows});
    let digest = drmr_core::snapshot::snapshot_digest(&private);
    let analysis_id = drmr_core::snapshot::analysis_id(&digest);
    private["snapshot_digest"] = json!(digest);
    private["analysis_id"] = json!(analysis_id);
    let path = dir.join("analysis.json");
    std::fs::write(&path, Dumps::SORTED.dumps(&private)).unwrap();
    Snapshot {
        analysis_id,
        digest,
        path,
        dir,
    }
}

fn submit_analysis(w: &Web, asset: &str) -> (Value, Snapshot) {
    submit_analysis_with(w, asset, &[video()])
}

fn submit_analysis_with(w: &Web, asset: &str, tracks: &[Track]) -> (Value, Snapshot) {
    let answer = w.post(
        "/api/hls/analyses",
        &json!({"asset_id": asset, "url": SOURCE_URL, "access_profile": "default"}),
    );
    assert_eq!(answer.status, 202, "{}", answer.text());
    let submitted = answer.json();
    let snapshot = write_snapshot(&w.h.output, asset, tracks);
    let job = submitted["job_id"].as_str().unwrap();
    w.finish(job, 0);
    assert_eq!(w.state(job), "succeeded");
    (submitted, snapshot)
}

// ---- the outputs the packaging child writes ---------------------------------------------------

/// `manifests/clear-hls/{manifest.mpd,master.m3u8}`, the package report, and an init segment for
/// every packaged video and audio track.
fn write_package_outputs(
    output: &Path,
    asset: &str,
    snapshot: &Snapshot,
    tracks: &[Track],
    not_selected: &[&str],
) {
    let root = output.join(asset);
    let manifests = root.join("manifests").join("clear-hls");
    support::private_dir(&manifests).unwrap();
    std::fs::write(
        manifests.join("manifest.mpd"),
        format!("<MPD xmlns:cenc=\"urn:mpeg:cenc:2013\"><ContentProtection cenc:default_KID=\"{KID}\"/></MPD>"),
    )
    .unwrap();
    std::fs::write(manifests.join("master.m3u8"), "#EXTM3U\n").unwrap();
    let mut rows = Vec::new();
    for track in tracks {
        let packaged = !not_selected.contains(&track.id.as_str());
        let disposition = if packaged {
            "packaged"
        } else {
            "duplicate-not-selected"
        };
        rows.push(json!({"source_track_id": track.id, "disposition": disposition, "checks": ["timing-pass"]}));
        if packaged && (track.content_type == "video" || track.content_type == "audio") {
            let init = root.join("cbcs").join(&track.id).join("init.mp4");
            support::private_dir(init.parent().unwrap()).unwrap();
            let data: &[u8] = if track.content_type == "video" {
                AVC_INIT
            } else {
                b"\x00\x00\x00\x08ftyp"
            };
            std::fs::write(init, data).unwrap();
        }
    }
    let report = json!({"success": true, "analysis_id": snapshot.analysis_id, "tracks": rows});
    std::fs::write(
        root.join("reports").join("package-hls-summary.json"),
        report.to_string(),
    )
    .unwrap();
}

fn package_body(analysis_id: &str, snapshot: &Snapshot, choice: &Value) -> Value {
    json!({"analysis_id": analysis_id,
           "selection": {"analysis_id": analysis_id, "snapshot_digest": snapshot.digest, "audio_source_id": choice},
           "drm": drm()})
}

fn submit_package(w: &Web) -> (String, Snapshot) {
    submit_package_with(w, "asset", &[video()], &Value::Null, &[])
}

fn submit_package_with(
    w: &Web,
    asset: &str,
    tracks: &[Track],
    choice: &Value,
    not_selected: &[&str],
) -> (String, Snapshot) {
    let (submitted, snapshot) = submit_analysis_with(w, asset, tracks);
    let analysis_id = submitted["analysis_id"].as_str().unwrap();
    let answer = w.post(
        "/api/hls/packages",
        &package_body(analysis_id, &snapshot, choice),
    );
    assert_eq!(answer.status, 202, "{}", answer.text());
    let job = answer.json()["job_id"].as_str().unwrap().to_string();
    write_package_outputs(&w.h.output, asset, &snapshot, tracks, not_selected);
    w.finish(&job, 0);
    assert_eq!(w.state(&job), "succeeded");
    (job, snapshot)
}

/// Every `*.json` file under `root`.
fn json_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "json") {
                files.push(path);
            }
        }
    }
    files
}

fn first_api_line(text: &str) -> String {
    text.lines()
        .find(|line| line.starts_with("/api/"))
        .unwrap()
        .to_string()
}

// ---- the cases ---------------------------------------------------------------------------------

#[test]
fn analysis_and_history_never_echo_source_or_secret_fields() {
    let w = web();
    let (submitted, _) = submit_analysis(&w, "asset");
    let analysis_id = submitted["analysis_id"].as_str().unwrap();
    let answer = w.get(&format!("/api/hls/analyses/{analysis_id}"));
    assert_eq!(answer.status, 200);
    let body = answer.json();
    assert_eq!(body["admitted"], true);
    assert_eq!(body["analysis_id"], analysis_id);
    assert_eq!(body["tracks"][0]["id"], "v000");
    let public = answer.text() + &w.get("/api/hls/jobs").text();
    let base = w.h.root.path().to_str().unwrap().to_string();
    assert!(
        !public.contains("PRIVATE") && !public.contains("127.0.0.1") && !public.contains(&base),
        "{public}"
    );
    let requests = w.h.output.join("asset/clear/_requests");
    assert!(
        json_files(&requests).is_empty(),
        "the request config is gone once the job ended"
    );
    let reports = w.h.output.join("asset/reports");
    let metadata = std::fs::read_dir(&reports)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("hls-job-")
        })
        .unwrap();
    assert!(
        !std::fs::read_to_string(metadata)
            .unwrap()
            .contains("PRIVATE")
    );
    assert_eq!(answer.header("cache-control"), Some("no-store"));
}

#[test]
fn all_three_drm_parameters_required_before_child_launch() {
    for field in [
        "kid",
        "key",
        "iv",
        "widevine_pssh",
        "playready_la_url",
        "fairplay_uri",
    ] {
        let w = web();
        let (submitted, _) = submit_analysis(&w, "asset");
        let mut data = drm();
        data.as_object_mut().unwrap().remove(field);
        let answer = w.post(
            "/api/hls/packages",
            &json!({"analysis_id": submitted["analysis_id"], "drm": data}),
        );
        assert_eq!(answer.status, 422, "{field}");
        assert_eq!(answer.json(), error("drm-parameters-invalid"));
        assert_eq!(w.h.cli.calls(), 1, "{field}: no child launched");
        assert!(!answer.text().contains(KEY) && !answer.text().contains(IV));
    }
}

#[test]
fn keys_only_flow_in_fresh_environment_not_argv_configs_history() {
    let w = web_with(|setup| setup.options.ambient = vec![("DRM_KEY", "ambient-secret")]);
    let (job, _) = submit_package(&w);
    // The analysis child got a fresh environment: no DRM variable at all, not even the ambient one.
    assert_eq!(w.h.cli.drm(0), ["", "", ""]);
    let package = w.h.cli.calls() - 1;
    assert_eq!(w.h.cli.drm(package), [KID, KEY, IV]);
    let argv = w.h.cli.argv(package).join(" ");
    for value in ["ambient-secret", KEY, IV] {
        assert!(!argv.contains(value), "{value} on argv");
    }
    let public = w.get(&format!("/api/hls/results/{job}")).text() + &w.get("/api/hls/jobs").text();
    for value in [KEY, IV, "PRIVATE", "license.example.invalid", FAIRPLAY] {
        assert!(!public.contains(value), "{value}");
    }
    for path in json_files(&w.h.output) {
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains(KEY) && !text.contains(IV),
            "{}",
            path.display()
        );
    }
    let result = w.get(&format!("/api/hls/results/{job}")).json();
    assert_eq!(
        (
            result["all_pass"].clone(),
            result["production_verified"].clone()
        ),
        (json!(true), json!(false))
    );
    assert_eq!(result["tracks"][0]["rep_id"], "v000");
}

#[test]
fn cross_origin_json_and_profile_boundaries_fail_closed() {
    let w = web();
    let body = json!({"asset_id": "evil", "url": PLAIN_URL});
    let client = &w.h.client;
    let foreign = client.post_json_with(
        "/api/hls/analyses",
        &body,
        &[("Origin", "https://evil.test")],
    );
    assert_eq!(
        (foreign.status, foreign.json()),
        (403, error("same-origin-required"))
    );
    let site = client.post_json_with(
        "/api/hls/analyses",
        &body,
        &[("Sec-Fetch-Site", "cross-site")],
    );
    assert_eq!(site.status, 403);
    let plain = client.post_raw(
        "/api/hls/analyses",
        body.to_string().as_bytes(),
        &[("Content-Type", "text/plain")],
    );
    assert_eq!((plain.status, plain.json()), (415, error("json-required")));
    for (key, value) in [
        ("url", "file:///etc/passwd"),
        ("url", "http://other.test/master.m3u8"),
        ("access_profile", "../secret"),
    ] {
        let mut changed = body.clone();
        changed[key] = json!(value);
        let answer = w.post("/api/hls/analyses", &changed);
        assert_eq!(
            (answer.status, answer.json()),
            (422, error("source-profile-invalid")),
            "{value}"
        );
    }
    assert_eq!(w.h.cli.calls(), 0);
}

#[test]
fn shared_manager_lease_refuses_second_mode_and_cleans_unowned_request() {
    let w = web();
    let holder = w.occupy_lease();
    let answer = w.post(
        "/api/hls/analyses",
        &json!({"asset_id": "asset", "url": PLAIN_URL}),
    );
    assert_eq!((answer.status, answer.json()), (409, error("job-busy")));
    assert!(!w.h.output.join("asset").exists());
    w.finish(&holder, 0);
}

#[test]
fn history_survives_restart_with_interrupted_jobs_and_safe_pagination() {
    let w = web();
    let (first, _) = submit_analysis(&w, "first");
    let answer = w.post(
        "/api/hls/analyses",
        &json!({"asset_id": "second", "url": PLAIN_URL}),
    );
    assert_eq!(answer.status, 202);
    let second = answer.json()["job_id"].as_str().unwrap().to_string();
    // A new server has no child handles and must not claim that old work is running.
    let (_server, client) = w.h.second();
    let page = client.get("/api/hls/jobs?limit=1").json();
    assert_eq!(page["jobs"][0]["state"], "interrupted");
    let cursor = page["next_cursor"].as_str().unwrap();
    let next = client
        .get(&format!("/api/hls/jobs?limit=1&cursor={cursor}"))
        .json();
    assert_eq!(next["jobs"][0]["job_id"], first["job_id"]);
    assert_eq!(next["jobs"][0]["state"], "succeeded");
    for query in ["limit=0", "limit=51", "limit=bad", "cursor=%%invalid"] {
        let answer = client.get(&format!("/api/hls/jobs?{query}"));
        assert_eq!(
            (answer.status, answer.json()),
            (400, error("pagination-invalid")),
            "{query}"
        );
    }
    let sse = client.get(&format!("/api/jobs/{second}/log")).text();
    assert!(
        sse.contains("event: done") && sse.contains("\"state\": \"interrupted\""),
        "{sse}"
    );
}

#[test]
fn session_key_scope_expiry_restart_and_source_preview_confinement() {
    let w = web();
    let (job, _) = submit_package(&w);
    let created = w.post("/api/hls/playback-sessions", &json!({"result_id": job}));
    assert_eq!(created.status, 201, "{}", created.text());
    let session = created.json();
    assert!(!created.text().contains(KEY) && !created.text().contains(IV));
    let old_url = session["old_url"].as_str().unwrap();
    let master = w.get(old_url);
    assert_eq!(master.status, 200);
    let text = master.text();
    assert!(
        text.contains("#EXTM3U") && !text.contains("file:") && !text.contains("PRIVATE"),
        "{text}"
    );
    let playlist = w.get(&first_api_line(&text)).text();
    let media =
        w.h.client
            .get_with(&first_api_line(&playlist), &[("Range", "bytes=0-9")]);
    assert_eq!((media.status, media.body.len()), (206, 10));
    let sibling = format!("{}/analysis.json", old_url.rsplit_once('/').unwrap().0);
    assert_eq!(w.get(&sibling).status, 404);
    assert_eq!(
        w.get("/output/asset/clear/source/analysis.json").status,
        404
    );
    let license_url = session["license_url"].as_str().unwrap();
    let license = w.h.client.post_raw(license_url, b"challenge", &[]);
    assert_eq!(license.status, 200);
    assert_eq!(license.header("cache-control"), Some("no-store"));
    assert_eq!(license.json()["keys"][0]["k"], b64url(KEY));
    let foreign = w.h.client.post_raw(
        license_url,
        b"challenge",
        &[("Origin", "https://evil.test")],
    );
    assert_eq!(foreign.status, 403);
    w.advance(1801.0);
    assert_eq!(w.h.client.post_raw(license_url, b"", &[]).status, 410);
    let expired = w.post("/api/hls/playback-sessions", &json!({"result_id": job}));
    assert_eq!(
        (expired.status, expired.json()),
        (410, error("playback-credentials-required"))
    );
    let wrong = w.post(
        "/api/hls/playback-sessions",
        &json!({"result_id": job, "kid": "ff".repeat(16), "key": KEY}),
    );
    assert_eq!(
        (wrong.status, wrong.json()),
        (422, error("playback-credentials-invalid"))
    );
    let restored = w.post(
        "/api/hls/playback-sessions",
        &json!({"result_id": job, "kid": KID, "key": KEY}),
    );
    assert_eq!(restored.status, 201);
    let restored = restored.json();
    assert_eq!(
        w.h.client
            .delete(&format!(
                "/api/hls/playback/{}",
                restored["session_id"].as_str().unwrap()
            ))
            .status,
        204
    );
    assert_eq!(
        w.h.client
            .post_raw(restored["license_url"].as_str().unwrap(), b"", &[])
            .status,
        410
    );
    let (_server, client) = w.h.second();
    assert_eq!(
        client
            .post_json("/api/hls/playback-sessions", &json!({"result_id": job}))
            .status,
        410
    );
}

#[test]
fn successful_result_is_immutable_across_duplicate_package_requests() {
    let w = web();
    let (job, _) = submit_package(&w);
    let analysis_id = w.get(&format!("/api/jobs/{job}")).json()["analysis_id"].clone();
    let answer = w.post(
        "/api/hls/packages",
        &json!({"analysis_id": analysis_id, "drm": drm()}),
    );
    assert_eq!(
        (answer.status, answer.json()),
        (409, error("result-already-exists"))
    );
    assert_eq!(w.get(&format!("/api/hls/results/{job}")).status, 200);
}

#[test]
fn unpublished_artifacts_and_untrusted_child_diagnostics_are_not_exposed() {
    let w = web();
    submit_analysis(&w, "asset");
    let leaked = w.h.output.join("asset/manifests/partial.mpd");
    support::private_dir(leaked.parent().unwrap()).unwrap();
    std::fs::write(&leaked, "unfinished").unwrap();
    assert_eq!(w.get("/output/asset/manifests/partial.mpd").status, 404);
    let line = json!({"stage": "analysis", "level": "error", "message": KEY, "url": "https://private/?token=PRIVATE", "key": KEY});
    let record = drmr_server::safelog::safe_log(&line.to_string()).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&record).unwrap()["message"],
        "job-progress"
    );
    assert!(!record.contains(KEY) && !record.contains("PRIVATE"));
    let kept = drmr_server::safelog::safe_log(r#"{"message":"non-independent-video"}"#).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&kept).unwrap()["message"],
        "non-independent-video"
    );
}

#[test]
fn live_analysis_progress_keeps_safe_classification_and_counters() {
    for message in [
        "analysis-started",
        "playlist-started",
        "segment-downloaded",
        "probe-started",
        "segment-probed",
        "playlist-complete",
        "analysis-complete",
    ] {
        let line = json!({"stage": "hls-source", "message": message, "rendition_id": "v000", "index": 1,
                          "total": 3, "track_count": 2, "url": format!("https://secret.test/?key={KEY}"),
                          "details": IV, "key": KEY});
        let safe: Value =
            serde_json::from_str(&drmr_server::safelog::safe_log(&line.to_string()).unwrap())
                .unwrap();
        assert_eq!(
            safe,
            json!({"stage": "hls-source", "level": "info", "message": message, "rendition_id": "v000",
                   "index": 1, "total": 3, "track_count": 2})
        );
    }
}

#[test]
fn durable_detail_report_is_repeatable_after_history_removal_and_restart() {
    let w = web();
    let (job, _) = submit_package(&w);
    let path = w.h.output.join("asset/reports/package-hls-summary.json");
    let mut report: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let first = report["tracks"][0].as_object_mut().unwrap();
    first.insert(
        "segments".into(),
        json!([{"source_index": 0, "output_index": 0, "timescale": 90000, "presentation_start": -1,
                "duration": 540_000, "sample_count": 180, "path": format!("/private/{KEY}"), "error": IV}]),
    );
    first.insert("key".into(), json!(KEY));
    first.insert("path".into(), json!("/private/path"));
    first["checks"]
        .as_array_mut()
        .unwrap()
        .extend([json!(KEY), json!("unknown-check-secret")]);
    report["error"] = json!(IV);
    std::fs::write(&path, report.to_string()).unwrap();
    let record = w.get(&format!("/api/jobs/{job}")).json();
    for _ in 0..2 {
        let answer = w.get(&format!("/api/hls/results/{job}"));
        assert_eq!(answer.status, 200);
        let body = answer.json();
        assert_eq!(body["analysis_id"], record["analysis_id"]);
        assert_eq!(body["created_at"], record["created_at"]);
        assert_eq!(
            body["track_reports"],
            json!([{"source_track_id": "v000", "disposition": "packaged",
                    "checks": [{"id": "timing-pass", "status": "PASS"}],
                    "segments": [{"source_index": 0, "output_index": 0, "timescale": 90000,
                                  "presentation_start": -1, "duration": 540_000, "sample_count": 180}]}])
        );
        let text = answer.text();
        assert!(!text.contains(KEY) && !text.contains(IV) && !text.contains("/private"));
    }
    let analysis_id = record["analysis_id"].as_str().unwrap();
    assert_eq!(
        w.h.client
            .delete(&format!("/api/hls/jobs/{analysis_id}"))
            .status,
        204
    );
    assert_eq!(
        w.h.client.delete(&format!("/api/hls/jobs/{job}")).status,
        204
    );
    let (_server, restarted) = w.h.second();
    assert_eq!(
        restarted.get(&format!("/api/hls/results/{job}")).status,
        200
    );
    let page = restarted.get(&format!("/reports/hls/{job}"));
    assert_eq!(page.status, 200);
    assert!(page.text().to_lowercase().contains("report"));
    assert_eq!(page.header("cache-control"), Some("no-store"));
    assert_eq!(restarted.get("/api/hls/jobs").json()["jobs"], json!([]));
    assert_eq!(w.h.cli.calls(), 2);
}

#[test]
fn report_page_rejects_unknown_and_unready_results() {
    let w = web();
    assert_eq!(w.get("/reports/hls/unknown").status, 404);
    assert_eq!(w.get("/reports/hls/unsafe%20id").status, 404);
    let (submitted, snapshot) = submit_analysis(&w, "asset");
    let analysis_job = submitted["job_id"].as_str().unwrap();
    assert_eq!(
        w.get(&format!("/reports/hls/{analysis_job}")).status,
        404,
        "an analysis is no result"
    );
    let analysis_id = submitted["analysis_id"].as_str().unwrap();
    let answer = w.post(
        "/api/hls/packages",
        &package_body(analysis_id, &snapshot, &Value::Null),
    );
    assert_eq!(answer.status, 202);
    let pending = answer.json()["job_id"].as_str().unwrap().to_string();
    w.finish(&pending, 1);
    assert_eq!(w.state(&pending), "failed");
    let page = w.get(&format!("/reports/hls/{pending}"));
    assert_eq!(
        (page.status, page.json()),
        (410, error("result-unavailable"))
    );
}

#[test]
fn malformed_child_progress_uses_inert_fallback() {
    let line = format!(
        r#"{{"stage": {{}}, "level": [], "message": {{"key": "{KEY}"}}, "duration_s": Infinity, "total": true, "index": -1, "rendition_id": "../{KEY}"}}"#
    );
    let safe: Value =
        serde_json::from_str(&drmr_server::safelog::safe_log(&line).unwrap()).unwrap();
    assert_eq!(
        safe,
        json!({"stage": "raw", "level": "info", "message": "job-progress"})
    );
}

#[test]
fn report_shell_does_not_repeat_expensive_snapshot_validation() {
    let w = web();
    let (job, snapshot) = submit_package(&w);
    assert_eq!(w.get(&format!("/api/hls/results/{job}")).status, 200);
    let segment = snapshot.dir.join("r000001.ts");
    let mut data = std::fs::read(&segment).unwrap();
    data.push(0);
    std::fs::write(&segment, data).unwrap();
    assert_eq!(w.get(&format!("/reports/hls/{job}")).status, 200);
    let api = w.get(&format!("/api/hls/results/{job}"));
    assert_eq!((api.status, api.json()), (410, error("result-unavailable")));
}

#[test]
fn missing_foreign_or_unknown_selection_bindings_are_rejected_before_launch() {
    for case in 0..4 {
        let w = web();
        let (submitted, snapshot) = submit_analysis(&w, "asset");
        let analysis_id = submitted["analysis_id"].as_str().unwrap();
        let selection = match case {
            0 => Value::Null,
            1 => json!({}),
            2 => json!({"analysis_id": "foreign", "snapshot_digest": "wrong"}),
            _ => {
                json!({"analysis_id": analysis_id, "snapshot_digest": snapshot.digest, "foreign": "ignored"})
            }
        };
        let answer = w.post(
            "/api/hls/packages",
            &json!({"analysis_id": analysis_id, "selection": selection, "drm": drm()}),
        );
        assert_eq!(
            (answer.status, answer.json()),
            (422, error("stale-packaging-selection")),
            "{selection}"
        );
        assert_eq!(w.h.cli.calls(), 1);
    }
}

#[test]
fn named_muxed_audio_and_independent_groups_match_pipeline_admission() {
    let w = web();
    let (submitted, _) = submit_analysis(&w, "asset");
    let url = format!(
        "/api/hls/analyses/{}",
        submitted["analysis_id"].as_str().unwrap()
    );
    let audio = |id: &str, group: &str| {
        track(id, "audio", "ts", "aac", vec![ts_segment()])
            .with("group_id", json!(group))
            .with("muxed", json!(true))
    };
    write_snapshot(
        &w.h.output,
        "asset",
        &[video(), audio("a1", "named"), audio("a2", "named")],
    );
    let result = w.get(&url).json();
    assert_eq!(result["requires_audio_selection"], true);
    let ids: Vec<&str> = result["audio_candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["a1", "a2"]);
    write_snapshot(
        &w.h.output,
        "asset",
        &[video(), audio("a1", "ja"), audio("a2", "en")],
    );
    let result = w.get(&url).json();
    assert_eq!(
        (
            result["admitted"].clone(),
            result["requires_audio_selection"].clone()
        ),
        (json!(true), json!(false))
    );
    write_snapshot(
        &w.h.output,
        "asset",
        &[
            video(),
            audio("a1", "ja"),
            audio("a2", "ja"),
            audio("a3", "en"),
        ],
    );
    let result = w.get(&url).json();
    assert_eq!(result["admitted"], false);
    assert_eq!(
        result["findings"][0]["code"],
        "multiple-audio-group-selection-required"
    );
}

/// `parse_attributes` of an `#EXT-X-MEDIA` line (quoted values may hold commas).
fn attributes(line: &str) -> Vec<(String, String)> {
    let list = line.split_once(':').unwrap().1;
    let pattern = regex::Regex::new(r#"([A-Z0-9-]+)=("[^"]*"|[^,]*)"#).unwrap();
    pattern
        .captures_iter(list)
        .map(|c| (c[1].to_string(), c[2].trim_matches('"').to_string()))
        .collect()
}

fn attribute<'a>(row: &'a [(String, String)], name: &str) -> &'a str {
    &row.iter().find(|(key, _)| key == name).unwrap().1
}

#[test]
fn source_preview_independent_audio_and_subtitles_use_stable_track_names() {
    let w = web();
    let main = video()
        .with("audio_group", json!("main-audio"))
        .with("subtitle_group", json!("subs"));
    let english = track("a099", "audio", "ts", "aac", vec![ts_segment()])
        .with("language", json!("en"))
        .with("group_id", json!("main-audio"));
    let japanese = track("a100", "audio", "ts", "aac", vec![ts_segment()])
        .with("language", json!("ja"))
        .with("group_id", json!("main-audio"))
        .with("is_default", json!(true))
        .with("autoselect", json!(true));
    let text = track(
        "t200",
        "text",
        "webvtt",
        "webvtt",
        vec![file("subtitle.vtt", b"WEBVTT\n")],
    )
    .with("language", json!("ja"))
    .with("group_id", json!("subs"))
    .with("is_default", json!(true))
    .with("autoselect", json!(true));
    let (job, _) = submit_package_with(
        &w,
        "asset",
        &[main, english, japanese, text],
        &Value::Null,
        &[],
    );
    let session = w
        .post("/api/hls/playback-sessions", &json!({"result_id": job}))
        .json();
    let master = w.get(session["old_url"].as_str().unwrap());
    assert_eq!(master.status, 200);
    let text = master.text();
    assert!(
        text.contains("TYPE=AUDIO") && text.contains("NAME=\"a100\""),
        "{text}"
    );
    assert!(text.contains("TYPE=SUBTITLES") && text.contains("NAME=\"t200\""));
    assert!(!text.contains("NAME=\"audio-0\"") && !text.contains("NAME=\"subtitle-0\""));
    let audio_rows: Vec<(String, String, String, String)> = text
        .lines()
        .filter(|line| line.starts_with("#EXT-X-MEDIA:TYPE=AUDIO,"))
        .map(|line| {
            let row = attributes(line);
            let get = |name| attribute(&row, name).to_string();
            (
                get("NAME"),
                get("LANGUAGE"),
                get("DEFAULT"),
                get("AUTOSELECT"),
            )
        })
        .collect();
    let expected = |name: &str, language: &str, flag: &str| {
        (name.into(), language.into(), flag.into(), flag.into())
    };
    assert_eq!(
        audio_rows,
        [expected("a099", "en", "NO"), expected("a100", "ja", "YES")]
    );
    let subtitle = attributes(
        text.lines()
            .find(|line| line.starts_with("#EXT-X-MEDIA:TYPE=SUBTITLES,"))
            .unwrap(),
    );
    assert_eq!(
        (
            attribute(&subtitle, "DEFAULT"),
            attribute(&subtitle, "AUTOSELECT")
        ),
        ("YES", "YES")
    );
    let master_path = w.h.root.path().join("sanitized-master.m3u8");
    std::fs::write(&master_path, &text).unwrap();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let run = std::process::Command::new("node")
        .args([
            "--test",
            "--test-name-pattern",
            "independent HLS audio",
            "tests/js/player.test.cjs",
        ])
        .current_dir(repository)
        .env("DRM_TEST_HLS_MASTER", &master_path)
        .output();
    match run {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {} // Node.js is not installed
        Err(error) => panic!("node: {error}"),
        Ok(output) => assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

#[test]
fn unadmitted_analysis_retry_reuses_id_and_preserves_history() {
    for state in ["failed", "interrupted"] {
        let mut w = web();
        let body = json!({"asset_id": "retry", "url": PLAIN_URL});
        let first = w.post("/api/hls/analyses", &body);
        assert_eq!(first.status, 202);
        let job = first.json()["job_id"].as_str().unwrap().to_string();
        if state == "failed" {
            w.finish(&job, 1);
        } else {
            // Restored metadata marks old work interrupted; no admitted snapshot exists.
            w.h.start();
        }
        assert_eq!(w.state(&job), state);
        let history = w.h.output.join(format!("retry/reports/hls-job-{job}.json"));
        let before = std::fs::read(&history).unwrap();
        let second = w.post("/api/hls/analyses", &body);
        assert_eq!(second.status, 202, "{state}: {}", second.text());
        assert_eq!(std::fs::read(&history).unwrap(), before);
        assert_eq!(w.job_ids().len(), 2);
    }
}

#[test]
fn retry_busy_cleanup_preserves_old_history_and_unknown_outputs_block_retry() {
    let w = web();
    let body = json!({"asset_id": "retry", "url": PLAIN_URL});
    let first = w.post("/api/hls/analyses", &body).json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    w.finish(&first, 1);
    w.get(&format!("/api/jobs/{first}"));
    let history =
        w.h.output
            .join(format!("retry/reports/hls-job-{first}.json"));
    let before = std::fs::read(&history).unwrap();
    let holder = w.occupy_lease();
    assert_eq!(w.post("/api/hls/analyses", &body).status, 409);
    assert_eq!(std::fs::read(&history).unwrap(), before);
    w.finish(&holder, 0);
    let unrelated = w.h.output.join("retry/cbcs");
    support::private_dir(&unrelated).unwrap();
    let blocked = w.post("/api/hls/analyses", &body);
    assert_eq!(
        (blocked.status, blocked.json()),
        (409, error("asset-already-exists"))
    );
    assert!(unrelated.is_dir());
}

#[test]
fn admitted_snapshot_and_unowned_asset_tree_never_allow_analysis_retry() {
    let w = web();
    let (_, snapshot) = submit_analysis(&w, "asset");
    let frozen = std::fs::read(&snapshot.path).unwrap();
    let body = json!({"asset_id": "asset", "url": PLAIN_URL});
    assert_eq!(w.post("/api/hls/analyses", &body).status, 409);
    assert_eq!(std::fs::read(&snapshot.path).unwrap(), frozen);
    support::private_dir(w.h.output.join("unowned")).unwrap();
    assert_eq!(
        w.post(
            "/api/hls/analyses",
            &json!({"asset_id": "unowned", "url": PLAIN_URL})
        )
        .status,
        409
    );
}

#[test]
fn opaque_fmp4_preview_uses_track_mime_for_head_get_and_ranges() {
    let w = web();
    let mut tracks = Vec::new();
    for (id, kind, codec) in [
        ("v100", "video", "h264"),
        ("a100", "audio", "aac"),
        ("t100", "text", "ttml"),
    ] {
        let container = if kind == "text" { "imsc" } else { "fmp4" };
        tracks.push(
            track(
                id,
                kind,
                container,
                codec,
                vec![file(
                    &format!("{id}-segment.m4s"),
                    b"\x00\x00\x00\x08moofsample-bytes",
                )],
            )
            .init(file(&format!("{id}-init.mp4"), b"\x00\x00\x00\x08ftyp")),
        );
    }
    let (job, snapshot) = submit_package_with(&w, "asset", &tracks, &Value::Null, &[]);
    let session = w
        .post("/api/hls/playback-sessions", &json!({"result_id": job}))
        .json();
    let master = w.get(session["old_url"].as_str().unwrap()).text();
    // Each track's playlist, named by the master: EXT-X-MEDIA rows by NAME, the variant by the
    // line after EXT-X-STREAM-INF.
    let mut playlists: Vec<(String, &str)> = Vec::new();
    let lines: Vec<&str> = master.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        if line.starts_with("#EXT-X-MEDIA:") {
            let row = attributes(line);
            let mime = if attribute(&row, "TYPE") == "AUDIO" {
                "audio/mp4"
            } else {
                "application/mp4"
            };
            playlists.push((attribute(&row, "URI").to_string(), mime));
        } else if line.starts_with("#EXT-X-STREAM-INF:") {
            playlists.push((lines[index + 1].to_string(), "video/mp4"));
        }
    }
    assert_eq!(playlists.len(), 3, "{master}");
    let mut checked = 0;
    let map = regex::Regex::new(r#"#EXT-X-MAP:URI="([^"]+)""#).unwrap();
    for (playlist_url, mime) in playlists {
        let playlist = w.get(&playlist_url).text();
        let mut media: Vec<(String, &str)> = map
            .captures_iter(&playlist)
            .map(|c| (c[1].to_string(), "init"))
            .collect();
        media.extend(
            playlist
                .lines()
                .filter(|line| line.starts_with("/api/"))
                .map(|line| (line.to_string(), "segment")),
        );
        for (url, part) in media {
            let resource_id = url.rsplit('/').next().unwrap();
            assert!(
                !resource_id.contains('.'),
                "the browser cannot infer a container from the opaque URL"
            );
            let expected = std::fs::read(snapshot.dir.join(if part == "init" {
                tracks
                    .iter()
                    .find(|t| mime_of(t) == mime)
                    .unwrap()
                    .init
                    .as_ref()
                    .unwrap()
                    .name
                    .clone()
            } else {
                tracks.iter().find(|t| mime_of(t) == mime).unwrap().segments[0]
                    .name
                    .clone()
            }))
            .unwrap();
            let head = w.h.client.request("HEAD", &url, &[], None);
            let body = w.get(&url);
            let ranged = w.h.client.get_with(&url, &[("Range", "bytes=0-3")]);
            assert_eq!((head.status, body.status), (200, 200));
            assert_eq!(
                (head.header("content-type"), body.header("content-type")),
                (Some(mime), Some(mime))
            );
            assert_eq!(body.body, expected);
            assert_eq!(
                (ranged.status, ranged.body.as_slice()),
                (206, &expected[..4])
            );
            assert_eq!(ranged.header("content-type"), Some(mime));
            checked += 1;
        }
    }
    assert_eq!(checked, 6);
}

fn mime_of(track: &Track) -> &'static str {
    match track.content_type.as_str() {
        "video" => "video/mp4",
        "audio" => "audio/mp4",
        _ => "application/mp4",
    }
}

#[test]
fn delete_terminal_history_is_durable_idempotent_and_key_free() {
    for state in ["succeeded", "failed", "interrupted"] {
        let mut w = web();
        let started = w.post(
            "/api/hls/analyses",
            &json!({"asset_id": "deletable", "url": PLAIN_URL}),
        );
        let job = started.json()["job_id"].as_str().unwrap().to_string();
        if state == "interrupted" {
            w.h.start();
        } else {
            w.finish(&job, i32::from(state != "succeeded"));
        }
        let log = w.h.output.join(format!("_jobs/{job}.jsonl"));
        std::fs::write(&log, "retained diagnostic log").unwrap();
        let response = w.h.client.delete(&format!("/api/hls/jobs/{job}"));
        assert_eq!(
            (response.status, response.body.as_slice()),
            (204, b"".as_slice()),
            "{state}"
        );
        assert_eq!(response.header("cache-control"), Some("no-store"));
        assert_eq!(
            w.h.client.delete(&format!("/api/hls/jobs/{job}")).status,
            204
        );
        assert_eq!(
            w.get("/api/hls/jobs").json(),
            json!({"jobs": [], "next_cursor": null})
        );
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "retained diagnostic log"
        );
        let public = w.get(&format!("/api/jobs/{job}")).json();
        assert_eq!(public["state"], state);
        assert!(public.get("hidden").is_none());
        let metadata =
            w.h.output
                .join(format!("deletable/reports/hls-job-{job}.json"));
        let stored: Value =
            serde_json::from_str(&std::fs::read_to_string(&metadata).unwrap()).unwrap();
        assert_eq!(stored["hidden"], true);
        let (server, client) = w.h.second();
        assert_eq!(client.get("/api/hls/jobs").json()["jobs"], json!([]));
        assert!(server.app.service.asset_reserved("deletable"));
        assert_eq!(client.delete(&format!("/api/hls/jobs/{job}")).status, 204);
    }
}

#[test]
fn running_history_cannot_be_deleted_and_refreshes_at_delete_time() {
    let w = web();
    let started = w.post(
        "/api/hls/analyses",
        &json!({"asset_id": "running", "url": PLAIN_URL}),
    );
    let job = started.json()["job_id"].as_str().unwrap().to_string();
    let response = w.h.client.delete(&format!("/api/hls/jobs/{job}"));
    assert_eq!(
        (response.status, response.json()),
        (409, error("job-running-cannot-delete"))
    );
    assert_eq!(w.job_ids(), std::slice::from_ref(&job));
    w.finish(&job, 0);
    assert_eq!(
        w.h.client.delete(&format!("/api/hls/jobs/{job}")).status,
        204
    );
    assert_eq!(w.job_ids(), Vec::<String>::new());
}

fn tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if !entry.file_name().to_str().unwrap().starts_with("hls-job-") {
                let data = std::fs::read(&path).unwrap();
                files.push((path, data));
            }
        }
    }
    files.sort();
    files
}

#[test]
fn deleted_parent_analysis_and_package_preserve_results_after_restart() {
    let w = web();
    let (job, _) = submit_package(&w);
    let parent = w.get(&format!("/api/jobs/{job}")).json()["analysis_id"]
        .as_str()
        .unwrap()
        .to_string();
    let before = tree(&w.h.output.join("asset"));
    assert_eq!(
        w.h.client.delete(&format!("/api/hls/jobs/{parent}")).status,
        204
    );
    assert_eq!(w.get(&format!("/api/hls/results/{job}")).status, 200);
    assert_eq!(
        w.h.client.delete(&format!("/api/hls/jobs/{job}")).status,
        204
    );
    let (server, client) = w.h.second();
    assert_eq!(client.get("/api/hls/jobs").json()["jobs"], json!([]));
    assert_eq!(
        client.get(&format!("/api/hls/analyses/{parent}")).json()["admitted"],
        true
    );
    let result = client.get(&format!("/api/hls/results/{job}"));
    assert_eq!(result.status, 200);
    assert_eq!(result.json()["all_pass"], true);
    assert_eq!(
        client
            .get(result.json()["dash_url"].as_str().unwrap())
            .status,
        200
    );
    let session = client.post_json(
        "/api/hls/playback-sessions",
        &json!({"result_id": job, "kid": KID, "key": KEY}),
    );
    assert_eq!(session.status, 201);
    assert_eq!(
        client
            .get(session.json()["old_url"].as_str().unwrap())
            .status,
        200
    );
    assert!(server.app.service.asset_reserved("asset"));
    assert!(server.app.service.output_published("asset").unwrap());
    let repeated = client.post_json(
        "/api/hls/analyses",
        &json!({"asset_id": "asset", "url": PLAIN_URL}),
    );
    assert_eq!(
        (repeated.status, repeated.json()),
        (409, error("asset-already-exists"))
    );
    let repeated = client.post_json(
        "/api/hls/packages",
        &json!({"analysis_id": parent, "drm": drm()}),
    );
    assert_eq!(
        (repeated.status, repeated.json()),
        (409, error("result-already-exists"))
    );
    assert_eq!(tree(&w.h.output.join("asset")), before);
}

#[test]
fn history_deletion_filters_before_pagination_and_keeps_existing_cursors() {
    let w = web();
    let jobs: Vec<String> = ["first", "second", "third"]
        .iter()
        .map(|asset| {
            submit_analysis(&w, asset).0["job_id"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    let first_page = w.get("/api/hls/jobs?limit=1").json();
    assert_eq!(first_page["jobs"][0]["job_id"], jobs[2]);
    assert_eq!(
        w.h.client
            .delete(&format!("/api/hls/jobs/{}", jobs[2]))
            .status,
        204
    );
    assert_eq!(
        w.h.client
            .delete(&format!("/api/hls/jobs/{}", jobs[1]))
            .status,
        204
    );
    let remaining = w.get("/api/hls/jobs?limit=1").json();
    assert_eq!(listed_ids(&remaining), jobs[..1]);
    assert!(remaining["next_cursor"].is_null());
    let cursor = first_page["next_cursor"].as_str().unwrap();
    let continued = w
        .get(&format!("/api/hls/jobs?limit=1&cursor={cursor}"))
        .json();
    assert_eq!(listed_ids(&continued), jobs[..1]);
    assert!(continued["next_cursor"].is_null());
}

#[test]
fn history_deletion_is_same_origin_and_does_not_follow_metadata_symlinks() {
    let w = web();
    let (submitted, _) = submit_analysis(&w, "asset");
    let job = submitted["job_id"].as_str().unwrap();
    let client = &w.h.client;
    let path = format!("/api/hls/jobs/{job}");
    assert_eq!(
        client
            .request("DELETE", &path, &[("Origin", "https://evil.test")], None)
            .status,
        403
    );
    assert_eq!(
        client
            .request("DELETE", &path, &[("Sec-Fetch-Site", "cross-site")], None)
            .status,
        403
    );
    assert_eq!(client.delete("/api/hls/jobs/missing").status, 404);
    assert_eq!(client.delete("/api/hls/jobs/..").status, 404);
    let reports = w.h.output.join("asset/reports");
    let outside = w.h.root.path().join("outside-reports");
    std::fs::rename(&reports, &outside).unwrap();
    support::directory_link(&outside, &reports);
    let metadata = outside.join(format!("hls-job-{job}.json"));
    let before = std::fs::read(&metadata).unwrap();
    let response = client.delete(&path);
    assert_eq!(
        (response.status, response.json()),
        (422, error("unsafe-output-path"))
    );
    assert_eq!(std::fs::read(&metadata).unwrap(), before);
    assert_eq!(w.job_ids(), [job.to_string()]);
}

#[test]
fn history_deletion_metadata_is_backward_compatible_and_explicitly_allowlisted() {
    let w = web();
    let (submitted, _) = submit_analysis(&w, "asset");
    let job = submitted["job_id"].as_str().unwrap();
    let metadata = w.h.output.join(format!("asset/reports/hls-job-{job}.json"));
    let mut old: Value =
        serde_json::from_str(&std::fs::read_to_string(&metadata).unwrap()).unwrap();
    old.as_object_mut().unwrap().remove("hidden");
    old["untrusted_field"] = json!("PRIVATE");
    old["drm_key"] = json!(KEY);
    std::fs::write(&metadata, old.to_string()).unwrap();
    let (_server, client) = w.h.second();
    let visible = client.get("/api/hls/jobs");
    assert_eq!(listed_ids(&visible.json()), [job.to_string()]);
    let text = visible.text();
    assert!(!text.contains("PRIVATE") && !text.contains(KEY) && !text.contains("hidden"));
    assert_eq!(client.delete(&format!("/api/hls/jobs/{job}")).status, 204);
    let mut stored: Value =
        serde_json::from_str(&std::fs::read_to_string(&metadata).unwrap()).unwrap();
    assert_eq!(stored["hidden"], true);
    assert!(stored.get("untrusted_field").is_none() && stored.get("drm_key").is_none());
    // An invalid hidden field must not become a truthy string hiding an active row.
    stored["hidden"] = json!("false");
    std::fs::write(&metadata, stored.to_string()).unwrap();
    let (third, again) = w.h.second();
    assert_eq!(
        again.get(&format!("/api/jobs/{job}")).status,
        404,
        "the record is not loaded"
    );
    assert!(!third.app.service.owns(job));
}

#[test]
fn failed_history_delete_write_keeps_row_visible_and_retryable() {
    if support::is_root() {
        return; // permissions are not enforced for root
    }
    let w = web();
    let (submitted, _) = submit_analysis(&w, "asset");
    let job = submitted["job_id"].as_str().unwrap();
    let reports = w.h.output.join("asset/reports");
    support::set_mode(&reports, 0o500);
    let failed = w.h.client.delete(&format!("/api/hls/jobs/{job}"));
    support::set_mode(&reports, 0o700);
    assert_eq!(failed.status, 500);
    assert_eq!(w.job_ids(), [job.to_string()]);
    assert_eq!(
        w.h.client.delete(&format!("/api/hls/jobs/{job}")).status,
        204
    );
    assert_eq!(w.job_ids(), Vec::<String>::new());
}

#[test]
fn hls_log_replays_after_restart_through_the_allowlist() {
    let w = web();
    let (submitted, _) = submit_analysis(&w, "replayed");
    let job = submitted["job_id"].as_str().unwrap();
    let log = w.h.output.join(format!("_jobs/{job}.jsonl"));
    let complete = r#"{"stage": "analysis", "level": "info", "message": "analysis-complete", "track_count": 1}"#;
    let raw = r#"{"stage": "raw", "level": "error", "message": "https://origin.test/secret?token=PRIVATE"}"#;
    std::fs::write(&log, format!("{complete}\n{raw}\n")).unwrap();
    let (_server, restarted) = w.h.second();
    let text = restarted.get(&format!("/api/jobs/{job}/log")).text();
    let events: Vec<&str> = text
        .split("\n\n")
        .filter(|block| !block.is_empty())
        .collect();
    assert_eq!(events[0], format!("data: {complete}"));
    assert!(
        !text.contains("PRIVATE") && !text.contains("origin.test"),
        "replayed lines pass the same allowlist"
    );
    let second: Value = serde_json::from_str(events[1].strip_prefix("data: ").unwrap()).unwrap();
    assert_eq!(second["message"], "job-progress");
    assert_eq!(
        *events.last().unwrap(),
        "event: done\ndata: {\"state\": \"succeeded\", \"exit_code\": 0}"
    );
    assert_eq!(sse_events(&text).len(), 3);
}

#[test]
fn concurrent_track_packaging_progress_keeps_safe_classification() {
    for message in ["track-started", "track-packaged", "track-failed"] {
        let level = if message == "track-failed" {
            "error"
        } else {
            "info"
        };
        let line = json!({"stage": "package-hls", "level": level, "message": message, "track_id": "v001",
                          "index": 1, "total": 5, "url": format!("https://secret.test/?key={KEY}"), "key": KEY});
        let safe: Value =
            serde_json::from_str(&drmr_server::safelog::safe_log(&line.to_string()).unwrap())
                .unwrap();
        assert_eq!(
            safe,
            json!({"stage": "package-hls", "level": level, "message": message, "track_id": "v001", "index": 1, "total": 5})
        );
    }
}

#[test]
fn a_profile_s_connection_and_probe_bounds_reach_the_analysis_policy() {
    let w = web();
    let mut profiles: Value =
        serde_json::from_str(&std::fs::read_to_string(&w.profiles).unwrap()).unwrap();
    let mut bounded = profiles["default"]["policy"].clone();
    bounded["connections"] = json!(3);
    bounded["probe_workers"] = json!(2);
    profiles["bounded"] = json!({"policy": bounded});
    std::fs::write(&w.profiles, profiles.to_string()).unwrap();
    let answer = w.post(
        "/api/hls/analyses",
        &json!({"asset_id": "bounded", "url": PLAIN_URL, "access_profile": "bounded"}),
    );
    assert_eq!(answer.status, 202, "{}", answer.text());
    let argv = w.h.cli.argv(0); // the held analysis child, once it has recorded itself
    assert_eq!(argv[..2], ["analyze-hls", "--source-config"]);
    let config: Value = serde_json::from_str(&std::fs::read_to_string(&argv[2]).unwrap()).unwrap();
    let policy = drmr_core::hls_cli::fetch_policy(&config["policy"]).unwrap();
    assert_eq!((policy.connections, policy.probe_workers), (3, Some(2)));
    assert_eq!(policy.allowed_local_roots, Vec::<PathBuf>::new());
}

#[test]
fn the_packaging_worker_bound_is_passed_to_the_cli_only_when_configured() {
    for (max_workers, expected) in [
        (None, Vec::<&str>::new()),
        (Some(4), vec!["--max-workers", "4"]),
    ] {
        let w = web_with(|setup| setup.options.max_workers = max_workers);
        submit_package(&w);
        let argv = w.last_argv();
        assert_eq!(argv[0], "package-hls");
        let bound: Vec<&str> = argv
            .iter()
            .map(String::as_str)
            .filter(|arg| {
                *arg == "--max-workers"
                    || (!arg.is_empty() && arg.bytes().all(|b| b.is_ascii_digit()))
            })
            .collect();
        assert_eq!(bound, expected);
    }
}

#[test]
fn a_request_body_over_the_limit_is_refused_before_any_work() {
    let w = web();
    let padding = "x".repeat(256 * 1024);
    let body = json!({"asset_id": "big", "url": PLAIN_URL, "padding": padding});
    let answer = w.post("/api/hls/analyses", &body);
    assert_eq!(
        (answer.status, answer.json()),
        (413, error("request-too-large"))
    );
    let exact = w.h.client.post_raw(
        "/api/queue",
        &vec![b' '; 256 * 1024],
        &[("Content-Type", "application/json")],
    );
    assert_eq!(
        exact.status, 400,
        "exactly the limit is read (and is not a JSON object)"
    );
    assert_eq!(w.h.cli.calls(), 0);
    assert!(!w.h.output.join("big").exists());
}

#[test]
fn stopping_analysis_removes_request_secrets_only_after_confirmed_completion() {
    let w = web();
    let answer = w.post(
        "/api/hls/analyses",
        &json!({"asset_id": "stopped-source", "url": SOURCE_URL}),
    );
    assert_eq!(answer.status, 202);
    let job_id = answer.json()["job_id"].as_str().unwrap().to_string();
    w.h.cli.wait_calls(1);
    let requests = w.h.output.join("stopped-source/clear/_requests");
    assert!(!json_files(&requests).is_empty());
    let stop = w.post(&format!("/api/jobs/{job_id}/stop"), &json!({}));
    assert!(matches!(stop.status, 200 | 202));
    assert_eq!(stop.json()["stop_requested"], true);
    let final_state = w.h.wait_job(&job_id);
    assert_eq!(final_state["state"], "interrupted");
    assert!(json_files(&requests).is_empty());
    assert_eq!(
        w.get("/api/hls/jobs").json()["jobs"][0]["state"],
        "interrupted"
    );
    assert!(!w.h.app().manager.is_busy());
}

#[test]
fn explicit_shutdown_confirms_analysis_cleanup_with_external_app_ownership() {
    let mut w = web();
    let response = w.post(
        "/api/hls/analyses",
        &json!({"asset_id": "close-active", "url": SOURCE_URL}),
    );
    assert_eq!(response.status, 202);
    let job_id = response.json()["job_id"].as_str().unwrap().to_string();
    w.h.cli.wait_calls(1);
    let requests = w.h.output.join("close-active/clear/_requests");
    assert!(!json_files(&requests).is_empty());
    let mut server = w.h.server.take().unwrap();
    let app = server.app.clone();
    server.shutdown().unwrap();
    assert_eq!(app.service.status(&job_id).unwrap()["state"], "interrupted");
    assert!(json_files(&requests).is_empty());
    assert!(!app.manager.is_busy());
    assert!(!app.queue.runner_active());
}
