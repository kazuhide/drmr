//! The UI is embedded at build time from `static/`: the embedded copy is byte for
//! byte the source tree (no file missing, none added), every file is served as it is, the page
//! shell is `/` and `/static/report.html`, and `--static-dir` serves a directory instead.

mod support;

use std::path::{Path, PathBuf};
use support::Harness;

fn static_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("static")
}

fn tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/");
                files.push((relative, std::fs::read(&path).unwrap()));
            }
        }
    }
    files.sort();
    files
}

#[test]
fn the_embedded_ui_is_byte_identical_to_static_assets() {
    let mut embedded: Vec<(String, Vec<u8>)> = drmr_server::static_files::embedded_files()
        .into_iter()
        .map(|(name, data)| (name, data.to_vec()))
        .collect();
    embedded.sort();
    let source = tree(&static_root());
    let names = |files: &[(String, Vec<u8>)]| {
        files
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&embedded), names(&source), "the same files");
    for ((name, data), (_, expected)) in embedded.iter().zip(&source) {
        assert!(data == expected, "{name} differs from static/{name}");
    }
    assert!(source.len() > 10 && source.iter().any(|(name, _)| name == "index.html"));
}

#[test]
fn every_embedded_file_is_served_byte_for_byte() {
    let h = Harness::new();
    for (name, data) in tree(&static_root()) {
        let answer = h.client.get(&format!("/static/{name}"));
        assert_eq!(answer.status, 200, "{name}");
        assert!(answer.body == data, "{name} served differently");
    }
    let index = std::fs::read(static_root().join("index.html")).unwrap();
    let page = h.client.get("/");
    assert_eq!(
        (page.status, page.header("content-type")),
        (200, Some("text/html; charset=utf-8"))
    );
    assert_eq!(page.body, index);
    let report = h.client.get("/static/report.html");
    assert_eq!(
        report.body,
        std::fs::read(static_root().join("report.html")).unwrap()
    );
    assert_eq!(h.client.get("/static/../keys.env").status, 404);
    assert_eq!(h.client.get("/static/nope.js").status, 404);
}

#[test]
fn a_static_directory_override_is_served_instead_of_the_embedded_copy() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("index.html"),
        "<!doctype html><title>dev</title>",
    )
    .unwrap();
    std::fs::write(dir.path().join("app.css"), "body{}").unwrap();
    let mut config =
        drmr_server::ServerConfig::new(dir.path().join("catalog"), dir.path().join("output"));
    std::fs::create_dir(&config.catalog_root).unwrap();
    config.bind_port = 0;
    config.static_dir = Some(dir.path().to_path_buf());
    let server = drmr_server::spawn(config).unwrap();
    let client = support::Client::new(&server.url());
    assert_eq!(client.get("/").text(), "<!doctype html><title>dev</title>");
    assert_eq!(client.get("/static/app.css").text(), "body{}");
    assert_eq!(
        client.get("/static/report.html").status,
        404,
        "only the directory is served"
    );
    server.stop().unwrap();
}
