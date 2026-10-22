use super::*;

fn private_plan_service(
    root: &std::path::Path,
) -> (
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
    drmr_server::RunningServer,
) {
    let input = root.join("input");
    let output = root.join("output");
    let private = root.join("private");
    support::private_dir(&input).unwrap();
    support::private_dir(&private).unwrap();
    let mut config = ServerConfig::new(&input, &output);
    config.common_config_root = Some(private.clone());
    config.bind_port = 0;
    config.hooks.log = std::sync::Arc::new(|_| Ok(()));
    (input, output, private, spawn(config).unwrap())
}

fn common_root(capability: &Value, label: &str) -> String {
    capability["roots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|root| root["label"] == label)
        .unwrap()["root_ref"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn saved_plan_import(
    client: &Client,
    run: &str,
    reference: &str,
    creation: u32,
) -> support::Answer {
    client.post_json("/api/common/sessions", &serde_json::json!({"schema_version":1,"server_id":run,"creation_request_id":format!("{creation:032x}"),"source":{"kind":"saved-plan","plan_file_ref":reference}}))
}

fn private_plan_bytes(input: &std::path::Path, output: &std::path::Path, schema: u32) -> Vec<u8> {
    use drmr_core::{
        conversion::{
            Converter, HlsInputKeys, InputKeys, VerificationSource,
            plan::{
                ExecutionPlanRequest,
                bundle::{BundleOutputIntent, BundlePlanRequest, ManifestValidation},
            },
        },
        events::NoSink,
        inspection::{OutputFormat, TargetProtection},
        toolrun::{Secrets, ToolRunner},
    };
    let converter = Converter::new(
        std::sync::Arc::new(ToolRunner::new(
            std::sync::Arc::new(Secrets::new()),
            std::sync::Arc::new(NoSink),
        )),
        "packager",
        "ffmpeg",
    );
    let input = VerificationSource {
        input,
        input_keys: &InputKeys::default(),
        hls_keys: &HlsInputKeys::default(),
    };
    match schema {
        1 => serde_json::to_vec_pretty(
            &converter
                .create_execution_plan(ExecutionPlanRequest {
                    input,
                    output,
                    format: Some(OutputFormat::Mp4),
                    target: TargetProtection::Clear,
                })
                .unwrap(),
        )
        .unwrap(),
        2 => serde_json::to_vec_pretty(
            &converter
                .create_bundle_plan(BundlePlanRequest {
                    input,
                    output_root: output,
                    outputs: &[BundleOutputIntent {
                        id: "clear".into(),
                        format: OutputFormat::Mp4,
                        target: TargetProtection::Clear,
                        drms: Vec::new(),
                        conformance: ManifestValidation::None,
                    }],
                })
                .unwrap(),
        )
        .unwrap(),
        _ => panic!("unsupported test plan schema"),
    }
}

fn copy_plan_input(path: &std::path::Path) {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb/clip.mp4");
    std::fs::copy(fixture, path).unwrap();
}

#[test]
fn private_saved_plan_imports_both_schemas_and_preserves_public_plan_selection() {
    for schema in [1, 2] {
        let root = tempfile::tempdir().unwrap();
        let (input_root, output, private, server) = private_plan_service(root.path());
        let client = Client::new(&server.url());
        let cap = client.get("/api/common/capabilities").json();
        let private_root = cap["roots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["label"] == "Private configuration folder")
            .unwrap();
        assert!(
            private_root["roles"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("saved-plan"))
        );
        let input_file = input_root.join("clip.mp4");
        copy_plan_input(&input_file);
        let bytes = private_plan_bytes(
            &input_file,
            &output.join(format!("result-v{schema}")),
            schema,
        );
        let expected: Value = serde_json::from_slice(&bytes).unwrap();
        let run = cap["server_id"].as_str().unwrap();
        for (index, (directory, label)) in [
            (&private, "Private configuration folder"),
            (&input_root, "Input folder"),
            (&output, "Output folder"),
        ]
        .into_iter()
        .enumerate()
        {
            let name = if index == 0 {
                "PRIVATE_SAVED_PLAN_NAME.json"
            } else {
                "saved-plan.json"
            };
            let path = directory.join(name);
            support::write_private(&path, &bytes).unwrap();
            drmr_core::conversion::plan::read_saved(&path).unwrap();
            let page = client.get(&format!(
                "/api/common/files?root_ref={}&role=saved-plan",
                common_root(&cap, label)
            ));
            assert_eq!(page.status, 200, "{}", page.text());
            let listing = page.json();
            let entries = listing["entries"].as_array().unwrap();
            assert_eq!(entries.len(), 1, "{}", page.text());
            assert_eq!(
                entries[0]["label"],
                if index == 0 { "Configuration 1" } else { name }
            );
            if index == 0 {
                assert!(!page.text().contains(name));
            }
            assert!(!page.text().contains(root.path().to_str().unwrap()));
            let created = saved_plan_import(
                &client,
                run,
                entries[0]["file_ref"].as_str().unwrap(),
                u32::try_from(index).unwrap() + 1,
            );
            assert_eq!(created.status, 201, "{}", created.text());
            assert!(!created.text().contains("PRIVATE_SAVED_PLAN_NAME"));
            assert!(!created.text().contains(root.path().to_str().unwrap()));
            let session = created.json();
            let plan_ref = session["plan_refs"][0].as_str().unwrap();
            let plan = client.get(&format!("/api/common/plans/{plan_ref}")).json();
            assert_eq!(plan["plan_schema_version"], schema);
            assert_eq!(plan["plan_id"], expected["plan_id"]);
            let exported = client.get(&format!("/api/common/plans/{plan_ref}/export"));
            assert_eq!(exported.status, 200);
            assert_eq!(exported.body, bytes);
        }
        server.stop().unwrap();
    }
}

#[test]
fn private_saved_plan_references_cannot_cross_roles_roots_or_replaced_ancestors() {
    let root = tempfile::tempdir().unwrap();
    let (_, _, private, server) = private_plan_service(root.path());
    let nested = private.join("PRIVATE_FOLDER");
    support::write_private(&nested.join("PRIVATE_PLAN.json"), b"{}").unwrap();
    support::write_private(&private.join("PRIVATE_CONFIG.json"), b"{}").unwrap();
    let client = Client::new(&server.url());
    let cap = client.get("/api/common/capabilities").json();
    let private_root = common_root(&cap, "Private configuration folder");
    let public_root = common_root(&cap, "Output folder");
    let run = cap["server_id"].as_str().unwrap();
    let listing = client.get(&format!(
        "/api/common/files?root_ref={private_root}&role=saved-plan"
    ));
    assert_eq!(listing.status, 200, "{}", listing.text());
    assert!(!listing.text().contains("PRIVATE_"));
    let page = listing.json();
    let entries = page["entries"].as_array().unwrap();
    let folder = entries
        .iter()
        .find(|entry| entry["kind"] == "directory")
        .unwrap()["directory_ref"]
        .as_str()
        .unwrap();
    let root_file = entries
        .iter()
        .find(|entry| entry["kind"] == "file")
        .unwrap()["file_ref"]
        .as_str()
        .unwrap();
    for query in [
        format!("root_ref={public_root}&role=saved-plan&directory_ref={folder}"),
        format!("root_ref={private_root}&role=keys&directory_ref={folder}"),
        format!("root_ref={private_root}&role=media"),
        format!("root_ref={private_root}&role=output-parent"),
    ] {
        assert_eq!(
            client.get(&format!("/api/common/files?{query}")).status,
            404,
            "{query}"
        );
    }
    let keys = client
        .get(&format!(
            "/api/common/files?root_ref={private_root}&role=keys"
        ))
        .json();
    let key_ref = keys["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["kind"] == "file")
        .unwrap()["file_ref"]
        .as_str()
        .unwrap();
    assert_eq!(saved_plan_import(&client, run, key_ref, 1).status, 404);
    let local = client.post_json("/api/common/sessions", &serde_json::json!({"schema_version":1,"server_id":run,"creation_request_id":"2".repeat(32),"source":{"kind":"local","input_ref":root_file}}));
    assert_eq!(local.status, 404);
    let nested_page =
        format!("/api/common/files?root_ref={private_root}&role=saved-plan&directory_ref={folder}");
    let selected = client.get(&nested_page).json()["entries"][0]["file_ref"]
        .as_str()
        .unwrap()
        .to_owned();
    std::fs::rename(nested.join("PRIVATE_PLAN.json"), nested.join("old.json")).unwrap();
    support::write_private(&nested.join("PRIVATE_PLAN.json"), b"{}").unwrap();
    assert_eq!(saved_plan_import(&client, run, &selected, 3).status, 404);
    let fresh_page = client.get(&nested_page).json();
    let fresh = fresh_page["entries"][0]["file_ref"].as_str().unwrap();
    std::fs::rename(&nested, private.join("old-folder")).unwrap();
    support::write_private(&nested.join("PRIVATE_PLAN.json"), b"{}").unwrap();
    assert_eq!(saved_plan_import(&client, run, fresh, 4).status, 404);
    assert_eq!(client.get(&nested_page).status, 404);
    std::fs::rename(&private, root.path().join("old-private")).unwrap();
    support::write_private(&private.join("PRIVATE_CONFIG.json"), b"{}").unwrap();
    assert_eq!(saved_plan_import(&client, run, root_file, 5).status, 404);
    assert_eq!(
        client
            .get(&format!(
                "/api/common/files?root_ref={private_root}&role=saved-plan"
            ))
            .status,
        404
    );
    server.stop().unwrap();
}

#[test]
fn private_saved_plan_import_revalidates_private_media_and_output_destinations() {
    let root = tempfile::tempdir().unwrap();
    let (input, output, private, server) = private_plan_service(root.path());
    copy_plan_input(&input.join("clip.mp4"));
    copy_plan_input(&private.join("clip.mp4"));
    let client = Client::new(&server.url());
    let cap = client.get("/api/common/capabilities").json();
    let private_root = common_root(&cap, "Private configuration folder");
    for (schema, source, destination) in [
        (1, private.join("clip.mp4"), output.join("public-result")),
        (2, input.join("clip.mp4"), private.join("private-result")),
    ] {
        let path = private.join("plan.json");
        let bytes = private_plan_bytes(&source, &destination, schema);
        support::write_private(&path, bytes).unwrap();
        drmr_core::conversion::plan::read_saved(&path).unwrap();
        let listed = client.get(&format!(
            "/api/common/files?root_ref={private_root}&role=saved-plan"
        ));
        assert_eq!(listed.status, 200, "{}", listed.text());
        let page = listed.json();
        let reference = page["entries"][0]["file_ref"].as_str().unwrap();
        let rejected = saved_plan_import(
            &client,
            cap["server_id"].as_str().unwrap(),
            reference,
            schema,
        );
        assert_eq!(rejected.status, 404, "{}", rejected.text());
        assert!(!destination.exists());
    }
    assert_eq!(
        client.get("/api/common/sessions").json()["sessions"]
            .as_array()
            .unwrap()
            .as_slice(),
        &[] as &[Value]
    );
    server.stop().unwrap();
}

#[test]
fn private_saved_plan_import_keeps_strict_private_reads_and_schema_validation() {
    let root = tempfile::tempdir().unwrap();
    let (_, _, private, server) = private_plan_service(root.path());
    let path = private.join("PRIVATE_PLAN.json");
    support::write_private(&path, b"{}").unwrap();
    let client = Client::new(&server.url());
    let cap = client.get("/api/common/capabilities").json();
    let page = client.get(&format!(
        "/api/common/files?root_ref={}&role=saved-plan",
        common_root(&cap, "Private configuration folder")
    ));
    assert_eq!(page.status, 200, "{}", page.text());
    let listed = page.json();
    let reference = listed["entries"][0]["file_ref"].as_str().unwrap();
    let run = cap["server_id"].as_str().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        support::set_mode(&path, 0o644);
        let denied = saved_plan_import(&client, run, reference, 1);
        assert_eq!(denied.status, 422);
        assert_eq!(denied.json()["error"]["code"], "private-config-unavailable");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        support::set_mode(&path, 0o600);
    }
    let malformed = saved_plan_import(&client, run, reference, 2);
    assert_eq!(malformed.status, 422);
    assert_eq!(malformed.json()["error"]["code"], "execution-plan-invalid");
    server.stop().unwrap();
}

#[test]
fn private_saved_plan_selection_cannot_browse_reserved_internal_artifacts() {
    let root = tempfile::tempdir().unwrap();
    let (input, output, private, server) = private_plan_service(root.path());
    for path in [
        input.join("_jobs/PRIVATE.json"),
        output.join("_jobs/PRIVATE.json"),
        output.join("_common/PRIVATE.json"),
        private.join("_jobs/PRIVATE.json"),
    ] {
        support::write_private(&path, b"PRIVATE_INTERNAL_CONTENT").unwrap();
    }
    support::write_private(&input.join("ordinary/saved-plan.json"), b"{}").unwrap();
    support::write_private(&private.join("ordinary/PRIVATE_PLAN.json"), b"{}").unwrap();
    let client = Client::new(&server.url());
    let cap = client.get("/api/common/capabilities").json();
    let input = client
        .get(&format!(
            "/api/common/files?root_ref={}&role=saved-plan",
            common_root(&cap, "Input folder")
        ))
        .json();
    assert_eq!(input["entries"].as_array().unwrap().len(), 1);
    assert_eq!(input["entries"][0]["label"], "ordinary");
    let output_page = client
        .get(&format!(
            "/api/common/files?root_ref={}&role=saved-plan",
            common_root(&cap, "Output folder")
        ))
        .json();
    assert!(
        output_page["entries"].as_array().unwrap().is_empty(),
        "{output_page}"
    );
    for role in ["saved-plan", "keys", "fetch-policy"] {
        let page = client.get(&format!(
            "/api/common/files?root_ref={}&role={role}",
            common_root(&cap, "Private configuration folder")
        ));
        assert_eq!(page.status, 200, "{}", page.text());
        assert!(!page.text().contains("PRIVATE"));
        let value = page.json();
        assert_eq!(value["entries"].as_array().unwrap().len(), 1);
        assert_eq!(value["entries"][0]["label"], "Folder 1");
        let child = client.get(&format!(
            "/api/common/files?root_ref={}&role={role}&directory_ref={}",
            common_root(&cap, "Private configuration folder"),
            value["entries"][0]["directory_ref"].as_str().unwrap()
        ));
        assert_eq!(child.status, 200);
        assert_eq!(child.json()["entries"][0]["label"], "Configuration 1");
        assert!(!child.text().contains("PRIVATE"));
    }
    server.stop().unwrap();
}

#[cfg(unix)]
#[test]
fn private_saved_plan_files_and_directories_cannot_be_selected_or_served_through_aliases() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("input");
    let entry = input.join("asset");
    support::private_dir(entry.join("download")).unwrap();
    let private = root.path().join("private");
    support::write_private(&private.join("PRIVATE_PLAN.json"), b"PRIVATE_PLAN_CONTENT").unwrap();
    let static_dir = root.path().join("static");
    std::fs::create_dir(&static_dir).unwrap();
    symlink(&private, static_dir.join("alias")).unwrap();
    symlink(&private, entry.join("download/alias")).unwrap();
    symlink(
        private.join("PRIVATE_PLAN.json"),
        input.join("public-plan.json"),
    )
    .unwrap();
    symlink(
        input.join("public-plan.json"),
        private.join("alias-plan.json"),
    )
    .unwrap();
    let mut config = ServerConfig::new(&input, root.path().join("output"));
    config.common_config_root = Some(private);
    config.static_dir = Some(static_dir);
    config.bind_port = 0;
    config.hooks.log = std::sync::Arc::new(|_| Ok(()));
    let server = spawn(config).unwrap();
    let client = Client::new(&server.url());
    let cap = client.get("/api/common/capabilities").json();
    let input = client.get(&format!(
        "/api/common/files?root_ref={}&role=saved-plan",
        common_root(&cap, "Input folder")
    ));
    assert_eq!(input.status, 200);
    assert!(!input.text().contains("public-plan.json"));
    let selected = client.get(&format!(
        "/api/common/files?root_ref={}&role=saved-plan",
        common_root(&cap, "Private configuration folder")
    ));
    assert_eq!(selected.status, 200, "{}", selected.text());
    assert_eq!(selected.json()["entries"].as_array().unwrap().len(), 1);
    assert!(!selected.text().contains("PRIVATE_PLAN"));
    for path in [
        "/static/alias/PRIVATE_PLAN.json",
        "/source/asset/alias/PRIVATE_PLAN.json",
    ] {
        let denied = client.get(path);
        assert_eq!(denied.status, 404, "{path}");
        assert!(!denied.text().contains("PRIVATE_PLAN_CONTENT"));
    }
    server.stop().unwrap();
}
