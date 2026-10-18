//! Real catalog producer receipts, current-generation output and stale-report refusal.
use drmr_core::{
    config::KeyMaterial,
    events::NoSink,
    native,
    toolrun::{RunOptions, Secrets, ToolRunner},
};
use drmr_server::{
    catalog::CatalogEntry,
    jobs::{ConversionJobManager, process_env},
};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "10101010101010101010101010101010";
const IV: &str = "20202020202020202020202020202020";
use drmr_server::{RunningServer, ServerConfig};
use serde_json::{Value, json};
use std::io::{Read as _, Write as _};
use std::net::TcpStream;

fn prepare_source(root: &Path) -> CatalogEntry {
    let source = root.join("download");
    native::create_media_dir_all(&source.join("video")).unwrap();
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    let objects =
        drmr_core::playready::build_playready_v43(KID, "https://license.example.invalid/rights")
            .unwrap();
    let runner = ToolRunner::new(Arc::new(Secrets::with([KEY, IV])), Arc::new(NoSink));
    let args = vec![
        "packager".to_string(),
        format!(
            "in={},stream=video,init_segment={},segment_template={},playlist_name=media.m3u8",
            clip.display(),
            source.join("video/init.mp4").display(),
            source.join("video/seg_$Number$.m4s").display()
        ),
        "--segment_duration".into(),
        "2".into(),
        "--fragment_duration".into(),
        "2".into(),
        "--enable_raw_key_encryption".into(),
        "--keys".into(),
        format!("key_id={KID}:key={KEY}"),
        "--clear_lead".into(),
        "0".into(),
        "--iv".into(),
        IV.into(),
        "--protection_scheme".into(),
        "cenc".into(),
        "--protection_systems".into(),
        "Widevine".into(),
        "--hls_master_playlist_output".into(),
        source.join("master.m3u8").display().to_string(),
        "--mpd_output".into(),
        source.join("source.mpd").display().to_string(),
        "--generate_static_live_mpd".into(),
    ];
    let generated = runner
        .run_tool(&args, "packager", &RunOptions::default())
        .unwrap();
    assert_eq!(generated.returncode, Some(0), "{}", generated.stderr_text());
    let playlist = source.join("media.m3u8");
    let hls = std::fs::read_to_string(&playlist).unwrap();
    let hls = hls
        .lines()
        .map(|line| {
            if line.starts_with("#EXT-X-KEY:") && !line.contains("IV=") {
                format!("{line},IV=0x{IV}")
            } else if line.starts_with("#EXT-X-VERSION:") {
                "#EXT-X-VERSION:7".into()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(playlist, hls).unwrap();
    // A public synthetic PlayReady header supplies the legacy catalog source's LA_URL.
    let mpd = source.join("source.mpd");
    let text = std::fs::read_to_string(&mpd).unwrap();
    let start = text.find("<MPD ").unwrap();
    let close = start + text[start..].find('>').unwrap();
    let text = format!(
        "{}<BaseURL>./</BaseURL>{}",
        &text[..=close],
        &text[close + 1..]
    );
    let text = text.replace(
        "<AdaptationSet ",
        "<AdaptationSet mimeType=\"video/mp4\" xmlns:cenc=\"urn:mpeg:cenc:2013\" ",
    );
    let start = text.find("<SegmentTemplate ").unwrap();
    let attr = start + text[start..].find("timescale=\"").unwrap() + "timescale=\"".len();
    let timescale = text[attr..attr + text[attr..].find('"').unwrap()]
        .parse::<u64>()
        .unwrap();
    let text = text.replace(
        "<SegmentTemplate ",
        &format!("<SegmentTemplate duration=\"{}\" ", timescale * 2),
    );
    let marker = "</AdaptationSet>";
    let protection = format!(
        "<ContentProtection schemeIdUri=\"urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95\"><cenc:pssh>{}</cenc:pssh><mspr:pro xmlns:mspr=\"urn:microsoft:playready\">{}</mspr:pro></ContentProtection>",
        objects.pssh_b64, objects.pro_b64
    );
    std::fs::write(&mpd, text.replace(marker, &(protection + marker))).unwrap();
    CatalogEntry {
        asset_id: "receipt-bbb".into(),
        label: "Public BBB".into(),
        mpd_path: mpd.clone(),
        hls_master_path: source.join("master.m3u8"),
        key_material: KeyMaterial::new(KID, KEY, IV).unwrap(),
        entry_dir: root.to_path_buf(),
    }
}

fn request(server: &RunningServer, method: &str, path: &str, body: &str) -> (u16, Vec<u8>) {
    let address = server.url().trim_start_matches("http://").to_string();
    let mut stream = TcpStream::connect(&address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let boundary = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let status = std::str::from_utf8(&response[9..12])
        .unwrap()
        .parse()
        .unwrap();
    (status, response[boundary + 4..].to_vec())
}
fn get(server: &RunningServer, path: &str) -> Value {
    let (status, bytes) = request(server, "GET", path, "");
    assert_eq!(status, 200, "{path}");
    serde_json::from_slice(&bytes).unwrap()
}
fn submit(server: &RunningServer) -> String {
    let (status, bytes) = request(
        server,
        "POST",
        "/api/convert",
        r#"{"asset_id":"receipt-bbb"}"#,
    );
    assert_eq!(status, 202, "{}", String::from_utf8_lossy(&bytes));
    serde_json::from_slice::<Value>(&bytes).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string()
}
fn wait(server: &RunningServer, id: &str) {
    let end = Instant::now() + Duration::from_mins(1);
    loop {
        let status = get(server, &format!("/api/jobs/{id}"));
        if status["state"] != "running" {
            let lines = server.app.manager.read_log(id).collect::<Vec<_>>();
            let verdicts = lines
                .iter()
                .filter(|l| !l.contains("tool-output") && !l.contains("\"event\": \"command\""))
                .map(|l| l.chars().take(500).collect::<String>())
                .collect::<Vec<_>>();
            assert_eq!(status["exit_code"], 0, "{verdicts:?}");
            return;
        }
        assert!(Instant::now() < end, "catalog job did not complete");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn source_hashes(root: &Path) -> Vec<(std::path::PathBuf, String)> {
    let mut paths = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                paths.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    drmr_core::snapshot::sha256_file(&path).unwrap(),
                ));
            }
        }
    }
    paths.sort();
    paths
}

#[test]
fn actual_catalog_completion_poll_qualifies_only_its_current_generation() {
    let temp = native::private_tempdir().unwrap();
    let catalog = temp.path().join("catalog");
    let entry = prepare_source(&catalog.join("entry"));
    native::create_private_new(&entry.entry_dir.join("keys.env"))
        .unwrap()
        .write_all(format!("DRM_KID={KID}\nDRM_KEY={KEY}\nDRM_IV={IV}\n").as_bytes())
        .unwrap();
    let descriptor = json!({"asset_id":"receipt-bbb","label":"Public BBB","mpd_path":"download/source.mpd","hls_master_path":"download/master.m3u8","keys_file":"keys.env"});
    native::create_private_new(&entry.entry_dir.join("asset.json"))
        .unwrap()
        .write_all(descriptor.to_string().as_bytes())
        .unwrap();
    let before = source_hashes(&entry.entry_dir);
    let output = temp.path().join("output");
    let mut config = ServerConfig::new(catalog, output.clone());
    config.bind_port = 0;
    config.cli_argv = vec![env!("CARGO_BIN_EXE_drmr").into()];
    config.hooks.monitor = false;
    config.hooks.log = Arc::new(|_| Ok(()));
    let server = drmr_server::spawn(config).unwrap();
    let manager = &server.app.manager;
    let id = submit(&server);
    wait(&server, &id);
    assert_eq!(
        get(&server, "/api/catalog")[0]["output_readiness"]["state"],
        "verified"
    );
    let assets = get(&server, "/api/assets/receipt-bbb");
    let url = assets["new_url"].as_str().unwrap();
    assert_eq!(request(&server, "GET", url, "").0, 200);
    let report = get(&server, "/api/reports/catalog/receipt-bbb");
    assert_eq!(report["all_pass"], true);
    assert_eq!(report["full_enumeration"], true);
    assert_eq!(report["round_trip_verified"], true);
    assert_eq!(report["output_readiness"]["state"], "verified");
    assert_eq!(report["production_verified"], false);
    assert_eq!(
        request(
            &server,
            "GET",
            "/output/receipt-bbb/cbcs/video/init.mp4",
            ""
        )
        .0,
        200
    );
    let current = manager.catalog_output("receipt-bbb", &output).unwrap();
    let restarted = ConversionJobManager::new(
        manager.jobs_dir(),
        vec![env!("CARGO_BIN_EXE_drmr").into()],
        process_env(),
    )
    .unwrap();
    assert_eq!(
        restarted.output_readiness("receipt-bbb", &output)["state"],
        "verified"
    );
    let manifest = output.join("receipt-bbb").join(&current.manifest);
    let original = std::fs::read(&manifest).unwrap();
    std::fs::write(&manifest, b"partial").unwrap();
    assert_eq!(
        get(&server, "/api/catalog")[0]["output_readiness"]["state"],
        "unverified"
    );
    assert!(get(&server, "/api/assets/receipt-bbb")["new_url"].is_null());
    assert_eq!(request(&server, "GET", url, "").0, 404);
    std::fs::write(&manifest, &original).unwrap();
    assert_eq!(
        restarted.output_readiness("receipt-bbb", &output)["state"],
        "unverified"
    );
    let report_path = output.join("receipt-bbb/reports/phase1-summary.json");
    let previous_report = std::fs::read(&report_path).unwrap();
    let next = submit(&server);
    wait(&server, &next);
    assert_ne!(next, id);
    assert_eq!(
        std::fs::read(&report_path).unwrap(),
        previous_report,
        "a genuine repeated success may have byte-identical report content"
    );
    assert_eq!(
        get(&server, "/api/catalog")[0]["output_readiness"]["job_id"],
        next
    );
    assert_eq!(
        get(&server, "/api/catalog")[0]["output_readiness"]["state"],
        "verified"
    );
    reject_receiptless_success(&server, &entry, &output, &previous_report);
    reject_failed_promotion(&server, &output);
    assert_eq!(source_hashes(&entry.entry_dir), before);
    server.stop().unwrap();
}

fn wait_native(manager: &ConversionJobManager, id: &str) {
    let end = Instant::now() + Duration::from_mins(1);
    loop {
        let status = manager.status(id).unwrap();
        if let Some(code) = status.exit_code {
            assert_eq!(code, 0);
            return;
        }
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn reject_receiptless_success(
    server: &RunningServer,
    entry: &CatalogEntry,
    output: &Path,
    previous: &[u8],
) {
    let control = output.join("fake-control");
    native::create_private_dir_all(&control.join("calls")).unwrap();
    let spec = drmr_test_support::ToolSpec {
        mode: drmr_test_support::Mode::ServerCli,
        ..Default::default()
    }
    .with("control_dir", control.to_str().unwrap());
    let program = drmr_test_support::install(&control.join("drmr"), &spec).unwrap();
    let fake = ConversionJobManager::new(
        server.app.manager.jobs_dir(),
        vec![program.into()],
        process_env(),
    )
    .unwrap();
    let handle = fake.submit(entry, output).unwrap();
    assert_eq!(
        server.app.manager.output_readiness(&entry.asset_id, output)["state"],
        "unverified"
    );
    wait_native(&fake, &handle.job_id);
    fake.qualify_catalog(&handle.job_id);
    assert_eq!(
        server.app.manager.output_readiness(&entry.asset_id, output)["state"],
        "unverified"
    );
    assert_eq!(
        std::fs::read(output.join("receipt-bbb/reports/phase1-summary.json")).unwrap(),
        previous
    );
    assert_eq!(
        request(server, "GET", "/api/reports/catalog/receipt-bbb", "").0,
        410
    );
    let restarted =
        ConversionJobManager::new(server.app.manager.jobs_dir(), vec![], process_env()).unwrap();
    assert_eq!(
        restarted.output_readiness(&entry.asset_id, output)["state"],
        "unverified"
    );
}
fn reject_failed_promotion(server: &RunningServer, output: &Path) {
    let id = submit(server);
    wait_native(&server.app.manager, &id);
    let records = std::fs::read_dir(server.app.manager.jobs_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("catalog-")
        })
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1);
    std::fs::remove_file(&records[0]).unwrap();
    native::create_private_dir_new(&records[0]).unwrap();
    server.app.manager.qualify_catalog(&id);
    assert_eq!(
        server.app.manager.output_readiness("receipt-bbb", output)["state"],
        "unverified"
    );
    let report: Value = serde_json::from_slice(
        &std::fs::read(output.join("receipt-bbb/reports/phase1-summary.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        report["all_pass"], true,
        "persistence failure does not delete completed outputs or promote readiness"
    );
}
