#![allow(dead_code)] // Each integration binary uses its own subset of the shared harness.
mod client_transport;
use drmr_server::{RunningServer, ServerConfig};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use ureq::unversioned::transport::Connector as _;

/// The fake CLI and its control directory. Every launch records its argv and DRM variables
/// under `calls/<n>/` (n = launch order, claimed with `mkdir`), prints the `lines` file, then:
/// with a `hold` file it waits for a `release` file (holding the exit code) and removes it; otherwise
/// it exits with the code in `code` (0 by default).
pub struct FakeCli {
    pub dir: PathBuf,
    pub script: PathBuf,
}

impl FakeCli {
    /// A fake CLI under `root/cli`.
    pub fn new(root: &Path) -> FakeCli {
        let dir = root.join("cli");
        drmr_core::native::create_private_dir_all(&dir.join("calls")).unwrap();
        let spec = drmr_test_support::ToolSpec {
            mode: drmr_test_support::Mode::ServerCli,
            ..Default::default()
        }
        .with("control_dir", dir.to_str().unwrap());
        let script = drmr_test_support::install(&dir.join("drmr"), &spec).unwrap();
        FakeCli { dir, script }
    }

    /// Lines the next launches print.
    pub fn lines(&self, lines: &[&str]) {
        let mut text = lines.join("\n");
        text.push('\n');
        std::fs::write(self.dir.join("lines"), text).unwrap();
    }

    /// Exit with this code (no hold).
    pub fn exit_code(&self, code: i32) {
        std::fs::write(self.dir.join("code"), code.to_string()).unwrap();
    }

    /// Hold every launch until [`FakeCli::release`].
    pub fn hold(&self) {
        std::fs::write(self.dir.join("hold"), "").unwrap();
    }

    /// Stop holding new launches.
    pub fn unhold(&self) {
        let _ = std::fs::remove_file(self.dir.join("hold"));
    }

    /// Let the held launch exit with this code.
    pub fn release(&self, code: i32) {
        let temporary = self.dir.join("release.pending");
        std::fs::write(&temporary, code.to_string()).unwrap();
        std::fs::rename(temporary, self.dir.join("release")).unwrap();
    }

    /// The number of launches so far.
    pub fn calls(&self) -> usize {
        std::fs::read_dir(self.dir.join("calls")).unwrap().count()
    }

    /// Wait until `count` launches have recorded themselves.
    pub fn wait_calls(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(15);
        wait_until(|| {
            if Instant::now() >= deadline {
                let calls: Vec<_> = std::fs::read_dir(self.dir.join("calls"))
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .collect();
                panic!("native CLI did not publish call readiness: {calls:?}");
            }
            self.calls() >= count
                && self
                    .dir
                    .join("calls")
                    .join((count - 1).to_string())
                    .join("inherited")
                    .exists()
        });
    }

    /// The argv of launch `n` (after the program).
    pub fn argv(&self, n: usize) -> Vec<String> {
        self.wait_calls(n + 1);
        read_lines(&self.dir.join("calls").join(n.to_string()).join("argv"))
    }

    /// `DRM_KID`, `DRM_KEY`, `DRM_IV` of launch `n` (empty when unset).
    pub fn drm(&self, n: usize) -> Vec<String> {
        self.wait_calls(n + 1);
        read_lines(&self.dir.join("calls").join(n.to_string()).join("drm"))
    }

    /// `PATH` and `AMBIENT_MARKER` of launch `n`.
    pub fn inherited(&self, n: usize) -> Vec<String> {
        self.wait_calls(n + 1);
        read_lines(&self.dir.join("calls").join(n.to_string()).join("inherited"))
    }
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Poll `condition` for up to 15 s.
pub fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !condition() {
        assert!(Instant::now() < deadline, "condition not met within 15 s");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// An HTTP answer.
#[derive(Debug)]
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Answer {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| panic!("not JSON: {}", self.text()))
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// A client for one server.
#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    pub base: String,
}

impl Client {
    pub fn new(base: &str) -> Client {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(30)))
            .build();
        let agent = ureq::Agent::with_parts(
            config,
            ureq::unversioned::transport::DefaultConnector::default()
                .chain(client_transport::ContinueReadConnector),
            ureq::unversioned::resolver::DefaultResolver::default(),
        );
        Client {
            agent,
            base: base.to_string(),
        }
    }

    /// Any request: method, path (sent as written), headers, body.
    pub fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Answer {
        let mut builder = ureq::http::Request::builder()
            .method(method)
            .uri(format!("{}{path}", self.base));
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        // Admission can reject a request before reading its body. Wait for the
        // server's decision instead of racing a large upload against its close.
        if body.is_some_and(|body| !body.is_empty())
            && !headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("expect"))
        {
            builder = builder.header("Expect", "100-continue");
        }
        let response = match body {
            Some(body) => self.agent.run(builder.body(body.to_vec()).unwrap()),
            None => self.agent.run(builder.body(()).unwrap()),
        };
        let mut response = response.unwrap_or_else(|error| panic!("{method} {path}: {error}"));
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let body = if method == "HEAD" {
            Vec::new()
        } else {
            response
                .body_mut()
                .with_config()
                .limit(u64::MAX)
                .read_to_vec()
                .unwrap_or_default()
        };
        Answer {
            status,
            headers,
            body,
        }
    }

    pub fn get(&self, path: &str) -> Answer {
        self.request("GET", path, &[], None)
    }

    pub fn get_with(&self, path: &str, headers: &[(&str, &str)]) -> Answer {
        self.request("GET", path, headers, None)
    }

    pub fn delete(&self, path: &str) -> Answer {
        self.request("DELETE", path, &[], None)
    }

    pub fn post_json(&self, path: &str, value: &Value) -> Answer {
        let body = serde_json::to_vec(value).unwrap();
        self.request(
            "POST",
            path,
            &[("Content-Type", "application/json")],
            Some(&body),
        )
    }

    pub fn post_json_with(&self, path: &str, value: &Value, headers: &[(&str, &str)]) -> Answer {
        let body = serde_json::to_vec(value).unwrap();
        let mut all = vec![("Content-Type", "application/json")];
        all.extend_from_slice(headers);
        self.request("POST", path, &all, Some(&body))
    }

    pub fn post_raw(&self, path: &str, body: &[u8], headers: &[(&str, &str)]) -> Answer {
        self.request("POST", path, headers, Some(body))
    }
}

/// A common console over owned temporary roots and a supervised native CLI fixture.
pub struct Harness {
    pub root: tempfile::TempDir,
    pub input: PathBuf,
    pub output: PathBuf,
    pub cli: FakeCli,
    pub log: Arc<Mutex<Vec<String>>>,
    pub server: Option<RunningServer>,
    pub client: Client,
}
impl Harness {
    pub fn new() -> Self {
        Self::with(|_| {})
    }
    pub fn with(configure: impl FnOnce(&mut ServerConfig)) -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("private");
        let input = base.join("input");
        let output = base.join("output");
        private_dir(&input).unwrap();
        private_dir(&output).unwrap();
        let cli = FakeCli::new(&base);
        let log = Arc::new(Mutex::new(Vec::new()));
        let retained_log = log.clone();
        let mut config = ServerConfig::new(&input, &output);
        config.bind_port = 0;
        config.cli_argv = vec![cli.script.clone().into_os_string()];
        config.hooks.log = Arc::new(move |line| {
            retained_log.lock().unwrap().push(line.into());
            Ok(())
        });
        configure(&mut config);
        let server = drmr_server::spawn(config).unwrap();
        let client = Client::new(&server.url());
        Self {
            root,
            input,
            output,
            cli,
            log,
            server: Some(server),
            client,
        }
    }
    pub fn app(&self) -> &Arc<drmr_server::App> {
        &self.server.as_ref().unwrap().app
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.stop().unwrap();
        }
    }
}

/// Start one common analysis using role-bound references, never raw child arguments.
pub fn start_analysis(h: &Harness, request: char) -> Value {
    let cap = h.client.get("/api/common/capabilities").json();
    let root = cap["roots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|root| root["label"] == "Input folder")
        .unwrap();
    let page = h
        .client
        .get(&format!(
            "/api/common/files?root_ref={}&role=media",
            root["root_ref"].as_str().unwrap()
        ))
        .json();
    let file = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["kind"] == "file")
        .unwrap();
    let session = h.client.post_json("/api/common/sessions", &json!({"schema_version":1,"server_id":cap["server_id"],"creation_request_id":request.to_string().repeat(32),"source":{"kind":"local","input_ref":file["file_ref"]}}));
    assert_eq!(session.status, 201, "{}", session.text());
    let started = h.client.post_json("/api/common/jobs", &json!({"schema_version":1,"server_id":cap["server_id"],"session_id":session.json()["session_id"],"request_id":request.to_string().repeat(32),"operation":{"kind":"analyze"},"runtime":{"keys_ref":null,"max_workers":1}}));
    assert_eq!(started.status, 202, "{}", started.text());
    started.json()
}
pub fn terminal(h: &Harness, job: &str) -> Value {
    let mut result = Value::Null;
    wait_until(|| {
        result = h.client.get(&format!("/api/common/jobs/{job}")).json();
        result["state"] != "running"
    });
    result
}

/// Whether the tests run as root (permission denials are not enforced then).
pub fn is_root() -> bool {
    // `id -u` rather than `libc::geteuid` (unsafe code is forbidden in this workspace).
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).trim() == "0")
}

/// Write a synthetic secret with the same private-file boundary as production.
pub fn write_private(path: &Path, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(parent) = path.parent() {
        drmr_core::native::create_private_dir_all(parent)?;
    }
    drmr_core::native::create_private_or_truncate(path)?.write_all(bytes.as_ref())
}

pub fn private_dir(path: impl AsRef<Path>) -> std::io::Result<()> {
    drmr_core::native::create_private_dir_all(path.as_ref())
}

#[cfg(unix)]
pub fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Apply actual Windows access denials while retaining access to restore the DACL.
#[cfg(windows)]
pub fn set_mode(path: &Path, mode: u32) {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_permissions::constants::{SeObjectType, SecurityInformation};
    use windows_permissions::{LocalBox, SecurityDescriptor};
    let mut control = std::fs::OpenOptions::new()
        .access_mode(0x0006_0000)
        .custom_flags(0x0200_0000)
        .open(path)
        .unwrap();
    let original = windows_permissions::wrappers::GetSecurityInfo(
        &control,
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Owner | SecurityInformation::Dacl,
    )
    .unwrap();
    let denied = match mode {
        0o000 => 0x0001_0067,
        0o500 => 0x0001_0046,
        _ => 0,
    };
    let sid = original.owner().unwrap();
    let deny = if denied == 0 {
        String::new()
    } else {
        format!("(D;;0x{denied:08x};;;{sid})")
    };
    let descriptor: LocalBox<SecurityDescriptor> =
        format!("D:P{deny}(A;;FA;;;{sid})").parse().unwrap();
    windows_permissions::wrappers::SetSecurityInfo(
        &mut control,
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
        None,
        None,
        descriptor.dacl(),
        None,
    )
    .unwrap();
}

#[cfg(unix)]
pub fn directory_link(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}
#[cfg(windows)]
pub fn directory_link(target: &Path, link: &Path) {
    let output = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(drmr_test_support::command_path(link).unwrap())
        .arg(drmr_test_support::command_path(target).unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "junction: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[cfg(unix)]
pub fn file_link(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}
#[cfg(windows)]
pub fn file_link(target: &Path, link: &Path) {
    std::fs::hard_link(target, link).unwrap();
}
