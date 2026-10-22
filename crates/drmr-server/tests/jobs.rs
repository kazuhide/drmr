//! Native process ownership exercised through the common HTTP workflow.
mod support;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use support::{Harness, start_analysis, terminal, wait_until};

fn input(h: &Harness) {
    std::fs::write(h.input.join("clip.mp4"), b"fixture input").unwrap();
}
fn id(job: &Value) -> &str {
    job["job_id"].as_str().unwrap()
}

#[test]
fn native_cli_readiness_covers_complete_captures_under_parallel_launches() {
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..4 {
                    let h = Harness::new();
                    input(&h);
                    h.cli.hold();
                    let job = start_analysis(&h, 'a');
                    h.cli.wait_calls(1);
                    let argv = h.cli.argv(0);
                    assert_eq!(argv[0], drmr_server::common_protocol::JOB_ARGUMENT);
                    assert_eq!(h.cli.drm(0), ["", "", ""]);
                    assert_eq!(h.cli.inherited(0).len(), 2);
                    assert_eq!(
                        h.client
                            .get(&format!("/api/common/jobs/{}", id(&job)))
                            .json()["supervision_confirmed"],
                        true
                    );
                    h.cli.release(0);
                    let ended = terminal(&h, id(&job));
                    assert_eq!(ended["exit_code"], 0);
                    assert_eq!(ended["cleanup_confirmed"], true);
                    // The native fixture produces no typed media result: zero exit alone never passes.
                    assert_eq!(ended["state"], "failed");
                }
            });
        }
    });
}
#[test]
fn completed_and_cancelled_jobs_clean_up_native_descendants() {
    for cancel in [false, true] {
        let h = Harness::new();
        input(&h);
        h.cli.hold();
        std::fs::write(h.cli.dir.join("tree"), b"").unwrap();
        let job = start_analysis(&h, 'a');
        wait_until(|| {
            std::fs::read_to_string(h.cli.dir.join("descendant.pid"))
                .is_ok_and(|text| text.ends_with('\n') && text.trim().parse::<u32>().is_ok())
        });
        assert!(descendant_running(&h.cli.dir));
        if cancel {
            let stopped = h
                .client
                .post_json(&format!("/api/common/jobs/{}/stop", id(&job)), &json!({}));
            assert!(matches!(stopped.status, 200 | 202));
        } else {
            h.cli.release(0);
        }
        let ended = terminal(&h, id(&job));
        wait_until(|| !descendant_running(&h.cli.dir));
        assert_eq!(ended["cleanup_confirmed"], true);
        assert_eq!(ended["supervision_confirmed"], true);
        assert_eq!(ended["stop_requested"], cancel);
    }
}
#[cfg(windows)]
fn descendant_running(directory: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    let result = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(directory.join("descendant.lock"));
    match result {
        Ok(_) => false,
        Err(error) if error.raw_os_error() == Some(32) => true,
        Err(error) => panic!("descendant lock inspection failed: {error}"),
    }
}

#[cfg(unix)]
fn descendant_running(directory: &Path) -> bool {
    let pid = std::fs::read_to_string(directory.join("descendant.pid")).unwrap();
    let output = std::process::Command::new("ps")
        .args(["-p", pid.trim(), "-o", "stat="])
        .output()
        .unwrap();
    let state = String::from_utf8(output.stdout).unwrap();
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}

#[cfg(windows)]
#[test]
fn windows_environment_names_cannot_bypass_key_isolation() {
    let h = Harness::with(|config| {
        config.hooks.base_env = Arc::new(|| {
            vec![
                ("drm_key".into(), "ambient".into()),
                ("Drm_Kid".into(), "ambient".into()),
                ("dRm_Iv".into(), "ambient".into()),
                ("AMBIENT_MARKER".into(), "retained".into()),
            ]
        })
    });
    assert_eq!(
        h.app().manager.fresh_env(),
        [("AMBIENT_MARKER".into(), "retained".into())]
    );
}
#[test]
fn common_launch_strips_ambient_keys_without_changing_the_parent_environment() {
    let before = std::env::vars_os().collect::<Vec<_>>();
    let h = Harness::with(|config| {
        config.hooks.base_env = Arc::new(|| {
            let mut values = std::env::vars_os().collect::<Vec<_>>();
            values.extend([
                ("DRM_KEY".into(), "PRIVATE_AMBIENT_KEY".into()),
                ("DRM_KID".into(), "PRIVATE_AMBIENT_ID".into()),
                ("DRM_IV".into(), "PRIVATE_AMBIENT_IV".into()),
                ("AMBIENT_MARKER".into(), "retained".into()),
            ]);
            values
        });
    });
    input(&h);
    h.cli.hold();
    let job = start_analysis(&h, 'a');
    h.cli.wait_calls(1);
    assert_eq!(h.cli.drm(0), ["", "", ""]);
    assert_eq!(h.cli.inherited(0)[1], "retained");
    assert!(
        h.cli
            .argv(0)
            .iter()
            .all(|arg| !arg.contains("PRIVATE_AMBIENT"))
    );
    assert_eq!(std::env::vars_os().collect::<Vec<_>>(), before);
    h.cli.release(0);
    terminal(&h, id(&job));
}
#[test]
fn stopping_a_replaced_job_never_stops_the_current_job() {
    let h = Harness::new();
    input(&h);
    h.cli.hold();
    let first = start_analysis(&h, 'a');
    h.cli.wait_calls(1);
    h.cli.release(0);
    terminal(&h, id(&first));
    let second = start_analysis(&h, 'b');
    h.cli.wait_calls(2);
    let stale = h
        .client
        .post_json(&format!("/api/common/jobs/{}/stop", id(&first)), &json!({}));
    assert_eq!(stale.status, 200);
    let current = h
        .client
        .get(&format!("/api/common/jobs/{}", id(&second)))
        .json();
    assert_eq!(current["state"], "running");
    assert_eq!(current["stop_requested"], false);
    h.cli.release(0);
    terminal(&h, id(&second));
}
#[test]
fn simultaneous_common_requests_admit_only_one_native_child() {
    let h = Harness::new();
    input(&h);
    h.cli.hold();
    let first = start_analysis(&h, 'a');
    h.cli.wait_calls(1);
    let cap = h.client.get("/api/common/capabilities").json();
    assert_eq!(cap["execution"]["availability"], "busy");
    let conflict = h.client.post_json("/api/common/jobs", &json!({"schema_version":1,"server_id":cap["server_id"],"session_id":first["session_id"],"request_id":"c".repeat(32),"operation":{"kind":"analyze"},"runtime":{"keys_ref":null,"max_workers":1}}));
    assert_eq!(conflict.status, 409);
    assert_eq!(h.cli.calls(), 1);
    h.cli.release(0);
    terminal(&h, id(&first));
}
