//! `S3StorageBackend`'s configuration from the environment, as botocore reads it: credentials
//! (`AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN`, then the profile files
//! named by `AWS_SHARED_CREDENTIALS_FILE` / `AWS_CONFIG_FILE` and `AWS_PROFILE`) and the
//! endpoint (`AWS_ENDPOINT_URL_S3`). Each probe runs in a child process with an explicit
//! environment and `HOME` in a temporary directory, so no real profile file is ever read
//! (`harness = false`: the environment of this process is never mutated). Synthetic
//! credentials only.

mod common;

use common::fake_s3::FakeS3;
use drmr_core::storage::{S3StorageBackend, StorageBackend};
use std::path::Path;

const PROBE: &str = "--s3-probe";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some(PROBE) {
        probe();
        return;
    }
    let cases: &[(&str, fn())] = &[
        (
            "environment_credentials_and_endpoint",
            environment_credentials_and_endpoint,
        ),
        ("profile_file_credentials", profile_file_credentials),
        (
            "unusable_or_missing_credentials",
            unusable_or_missing_credentials,
        ),
        (
            "environment_proxies_are_not_used",
            environment_proxies_are_not_used,
        ),
    ];
    let filter = args.iter().find(|arg| !arg.starts_with('-'));
    let mut passed = 0;
    let mut failed = Vec::new();
    for (name, case) in cases
        .iter()
        .filter(|(n, _)| filter.is_none_or(|f| n.contains(f.as_str())))
    {
        if std::panic::catch_unwind(case).is_ok() {
            println!("test {name} ... ok");
            passed += 1;
        } else {
            println!("test {name} ... FAILED");
            failed.push(*name);
        }
    }
    let verdict = if failed.is_empty() { "ok" } else { "FAILED" };
    println!(
        "\ntest result: {verdict}. {passed} passed; {} failed; 0 ignored; 0 measured; 0 filtered out",
        failed.len()
    );
    if !failed.is_empty() {
        std::process::exit(101);
    }
}

/// In the child: build the backend for bucket `b` and print `ok` or the error token, then the
/// result of one `exists` call.
fn probe() {
    match S3StorageBackend::new("b", "ap-northeast-1") {
        Ok(backend) => {
            let exists = backend
                .exists(&drmr_core::storage::ObjectRef::new("output/x").unwrap())
                .map_or_else(|e| e.token().to_string(), |found| found.to_string());
            println!("ok {exists}");
        }
        Err(error) => println!("{}", error.token()),
    }
}

/// Run the probe with exactly these variables (plus `PATH`); `HOME` is a temporary directory.
fn run_probe(home: &Path, variables: &[(&str, String)]) -> String {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.arg(PROBE).env_clear().env("HOME", home);
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    #[cfg(windows)]
    for name in ["SystemRoot", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    for (name, value) in variables {
        command.env(name, value);
    }
    let output = command.output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn start_fake() -> FakeS3 {
    let fake = FakeS3::start();
    fake.bucket("b", "ap-northeast-1");
    fake
}

/// The `X-Amz-Credential` / `X-Amz-Security-Token` of the signed requests.
fn signatures(fake: &FakeS3) -> Vec<(String, Option<String>)> {
    fake.requests()
        .iter()
        .map(|request| {
            (
                request
                    .query("X-Amz-Credential")
                    .unwrap_or_default()
                    .to_string(),
                request.query("X-Amz-Security-Token").map(str::to_string),
            )
        })
        .collect()
}

fn environment_credentials_and_endpoint() {
    let home = tempfile::tempdir().unwrap();
    let fake = start_fake();
    let output = run_probe(
        home.path(),
        &[
            ("AWS_ENDPOINT_URL_S3", fake.endpoint.clone()),
            ("AWS_ACCESS_KEY_ID", "AKIDENVSYNTHETIC".to_string()),
            ("AWS_SECRET_ACCESS_KEY", "synthetic-env-secret".to_string()),
            ("AWS_SESSION_TOKEN", "synthetic-session".to_string()),
        ],
    );
    assert_eq!(output, "ok false");
    let signed = signatures(&fake);
    assert_eq!(signed.len(), 2, "GetBucketLocation, then HeadObject");
    for (credential, token) in signed {
        assert!(credential.starts_with("AKIDENVSYNTHETIC/"), "{credential}");
        assert!(
            credential.ends_with("/ap-northeast-1/s3/aws4_request"),
            "{credential}"
        );
        assert_eq!(token.as_deref(), Some("synthetic-session"));
    }
    // AWS_ENDPOINT_URL is the fallback of AWS_ENDPOINT_URL_S3.
    let fallback = start_fake();
    let output = run_probe(
        home.path(),
        &[
            ("AWS_ENDPOINT_URL", fallback.endpoint.clone()),
            ("AWS_ACCESS_KEY_ID", "AKIDENVSYNTHETIC".to_string()),
            ("AWS_SECRET_ACCESS_KEY", "synthetic-env-secret".to_string()),
        ],
    );
    assert_eq!(output, "ok false");
    assert!(
        signatures(&fallback)
            .iter()
            .all(|(_, token)| token.is_none())
    );
}

fn environment_proxies_are_not_used() {
    let home = tempfile::tempdir().unwrap();
    let fake = start_fake();
    let proxy = start_fake();
    let output = run_probe(
        home.path(),
        &[
            ("AWS_ENDPOINT_URL_S3", fake.endpoint.clone()),
            ("AWS_ACCESS_KEY_ID", "AKIDENVSYNTHETIC".to_string()),
            ("AWS_SECRET_ACCESS_KEY", "synthetic-env-secret".to_string()),
            ("HTTP_PROXY", proxy.endpoint.clone()),
            ("http_proxy", proxy.endpoint.clone()),
            ("HTTPS_PROXY", proxy.endpoint.clone()),
            ("ALL_PROXY", proxy.endpoint.clone()),
            ("NO_PROXY", String::new()),
        ],
    );
    assert_eq!(output, "ok false");
    assert_eq!(fake.requests().len(), 2);
    assert!(proxy.requests().is_empty());
}

fn profile_file_credentials() {
    let home = tempfile::tempdir().unwrap();
    let credentials = home.path().join("credentials");
    std::fs::write(
        &credentials,
        "[default]\naws_access_key_id = AKIDDEFAULTSYNTH\naws_secret_access_key = d\n\n\
         # comment\n[batch]\nAWS_ACCESS_KEY_ID = AKIDPROFILESYNTH\naws_secret_access_key: p\n",
    )
    .unwrap();
    let config = home.path().join("config");
    std::fs::write(
        &config,
        "[profile other]\naws_access_key_id = AKIDCONFIGSYNTH\naws_secret_access_key = c\n",
    )
    .unwrap();
    let files = |profile: Option<&str>, endpoint: &str| {
        let mut variables = vec![
            ("AWS_ENDPOINT_URL_S3", endpoint.to_string()),
            (
                "AWS_SHARED_CREDENTIALS_FILE",
                credentials.display().to_string(),
            ),
            ("AWS_CONFIG_FILE", config.display().to_string()),
        ];
        if let Some(profile) = profile {
            variables.push(("AWS_PROFILE", profile.to_string()));
        }
        variables
    };
    for (profile, key) in [
        (None, "AKIDDEFAULTSYNTH/"),
        (Some("batch"), "AKIDPROFILESYNTH/"),
        (Some("other"), "AKIDCONFIGSYNTH/"),
    ] {
        let fake = start_fake();
        assert_eq!(
            run_probe(home.path(), &files(profile, &fake.endpoint)),
            "ok false"
        );
        assert!(
            signatures(&fake)
                .iter()
                .all(|(credential, _)| credential.starts_with(key)),
            "{profile:?}"
        );
    }
    // A named profile that exists in neither file: botocore cannot create the client.
    let fake = start_fake();
    assert_eq!(
        run_probe(home.path(), &files(Some("absent"), &fake.endpoint)),
        "storage-client-unavailable"
    );
    assert!(fake.requests().is_empty());
}

fn unusable_or_missing_credentials() {
    let home = tempfile::tempdir().unwrap();
    let nowhere = home.path().join("missing");
    let fake = start_fake();
    // No credentials anywhere: every request fails unsigned (NoCredentialsError), not retried.
    let output = run_probe(
        home.path(),
        &[
            ("AWS_ENDPOINT_URL_S3", fake.endpoint.clone()),
            ("AWS_SHARED_CREDENTIALS_FILE", nowhere.display().to_string()),
            ("AWS_CONFIG_FILE", nowhere.display().to_string()),
        ],
    );
    assert_eq!(output, "storage-failed");
    assert!(fake.requests().is_empty(), "nothing is sent unsigned");
    // Half a key pair in the environment: the client cannot be created.
    let output = run_probe(
        home.path(),
        &[
            ("AWS_ENDPOINT_URL_S3", fake.endpoint.clone()),
            ("AWS_ACCESS_KEY_ID", "AKIDHALFSYNTHETIC".to_string()),
        ],
    );
    assert_eq!(output, "storage-client-unavailable");
    // An endpoint that is not a URL: the client cannot be created either.
    let output = run_probe(
        home.path(),
        &[
            ("AWS_ENDPOINT_URL_S3", "not a url".to_string()),
            ("AWS_ACCESS_KEY_ID", "AKIDENVSYNTHETIC".to_string()),
            ("AWS_SECRET_ACCESS_KEY", "synthetic-env-secret".to_string()),
        ],
    );
    assert_eq!(output, "storage-client-unavailable");
    // A region other than ap-northeast-1 is refused before any of this is read.
    assert_eq!(
        S3StorageBackend::new("b", "eu-west-1").unwrap_err().token(),
        "storage-region-invalid"
    );
    let _: Option<&dyn StorageBackend> = None;
}
