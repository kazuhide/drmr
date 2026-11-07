//! Public editing examples remain accepted by the strict runtime configuration parsers.
mod common;

use drmr_core::conversion::{
    key_file::read_bundle_diagnostic,
    plan::bundle::{parse_output_config_diagnostic, validate_output_keys},
};
use serde::Deserialize;
use std::{io::Write as _, path::Path};

#[derive(Deserialize)]
struct Corpus {
    schema_version: u32,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    file: String,
    parser_valid: bool,
    document: Option<serde_json::Value>,
    json: Option<String>,
}

fn protected(path: &Path, bytes: &[u8]) {
    drmr_core::native::create_private_or_truncate(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

#[test]
fn shared_schema_corpus_matches_strict_rust_parsers() {
    let corpus: Corpus =
        serde_json::from_str(include_str!("fixtures/configuration/corpus.json")).unwrap();
    assert_eq!(corpus.schema_version, 1);
    let root = common::private_tempdir().unwrap();
    let key_file = root.path().join("keys.json");
    for case in corpus.cases {
        let bytes = case.json.map_or_else(
            || serde_json::to_vec(case.document.as_ref().unwrap()).unwrap(),
            String::into_bytes,
        );
        let parsed = match case.file.as_str() {
            "outputs" => parse_output_config_diagnostic(&bytes).map(|_| ()),
            "keys" => {
                protected(&key_file, &bytes);
                read_bundle_diagnostic(&key_file).map(|_| ())
            }
            _ => panic!("unknown corpus file kind"),
        };
        assert_eq!(parsed.is_ok(), case.parser_valid, "{}", case.name);
        if let Err(error) = parsed {
            assert!(!format!("{error:?}").contains("PRIVATE_EDITOR_CANARY"));
            assert!(!error.diagnostic.message().contains("PRIVATE_EDITOR_CANARY"));
        }
    }
}

#[test]
fn generated_template_pairs_pass_runtime_output_key_checks() {
    let root = common::private_tempdir().unwrap();
    let key_file = root.path().join("keys.json");
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/templates");
    for name in ["clear", "encrypted", "mixed4"] {
        let output_bytes = std::fs::read(directory.join(format!("{name}-outputs.json"))).unwrap();
        let outputs = parse_output_config_diagnostic(&output_bytes).unwrap();
        let key_bytes = std::fs::read(directory.join(format!("{name}-keys.json"))).unwrap();
        protected(&key_file, &key_bytes);
        let keys = read_bundle_diagnostic(&key_file).unwrap();
        validate_output_keys(&outputs.outputs, &keys).unwrap();
        let serialized = serde_json::to_value(&outputs).unwrap();
        assert!(serialized.get("$schema").is_none());
    }
}
