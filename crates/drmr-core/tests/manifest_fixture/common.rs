//! Synthetic signaling values and committed expected results for manifest tests.

use drmr_core::playready::PlayReadyObjects;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The synthetic KID, as a UUID.
pub const KID_UUID: &str = "00010203-0405-0607-0809-0a0b0c0d0e0f";
/// The synthetic KID, as hex.
pub const KID_HEX: &str = "000102030405060708090a0b0c0d0e0f";
/// The synthetic IV.
pub const IV_HEX: &str = "20202020202020202020202020202020";
/// The synthetic `FairPlay` URI.
pub const FAIRPLAY_URI: &str = "skd://sample-content-id";

/// A fixture JSON file of `tests/fixtures/manifests/`.
pub fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/manifests")
        .join(name);
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// A string field of a fixture.
pub fn text(value: &Value) -> &str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("not a string: {value}"))
}

/// `build_playready_v43(KID, LA_URL)` as Python built it.
pub fn playready() -> PlayReadyObjects {
    let objects = &fixture("manifest_dash.json")["playready"];
    PlayReadyObjects {
        wrmheader_xml: text(&objects["wrmheader_xml"]).to_string(),
        pro_bytes: hex::decode(text(&objects["pro_bytes"])).unwrap(),
        pssh_box: hex::decode(text(&objects["pssh_box"])).unwrap(),
        pro_b64: text(&objects["pro_b64"]).to_string(),
        pssh_b64: text(&objects["pssh_b64"]).to_string(),
        pssh_hex: text(&objects["pssh_hex"]).to_string(),
    }
}

/// `[class, message]` of a fixture error.
pub fn error_of(value: &Value) -> (String, String) {
    (text(&value[0]).to_string(), text(&value[1]).to_string())
}

/// `Path.read_text` of a file the test wrote or the code under test wrote.
pub fn read(path: &Path) -> String {
    String::from_utf8(std::fs::read(path).unwrap()).unwrap()
}

/// Every regular file under `root`, as sorted POSIX relative paths.
pub fn files_under(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let relative = path.strip_prefix(root).unwrap();
                out.push(relative.to_str().unwrap().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}
