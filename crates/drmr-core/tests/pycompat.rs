//! `CPython`-compatible parsing and serialization against committed reference fixtures.

use drmr_core::pycompat::{
    MAX_XML_DEPTH, PurePosixPath, b64decode, decode_utf16le, etree_get, etree_parse, etree_text,
    fnmatch, fromhex, glob, int, repr_int_list, repr_str, resolve, strip, urljoin, urlparse,
    xml_line_ends,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn oracle() -> Value {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pycompat.oracle.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn cases<'a>(oracle: &'a Value, name: &str) -> &'a Vec<Value> {
    oracle[name].as_array().unwrap()
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap()
}

/// A fixture outcome (`{"ok":...}` or `{"error":...}`) as a `Result` of strings.
fn expected(outcome: &Value) -> Result<String, String> {
    match outcome.get("ok") {
        Some(Value::String(value)) => Ok(value.clone()),
        Some(other) => Ok(other.to_string()),
        None => Err(text(&outcome["error"]).to_string()),
    }
}

#[test]
fn strip_is_str_strip() {
    let oracle = oracle();
    for case in cases(&oracle, "strip") {
        assert_eq!(strip(text(&case[0])), text(&case[1]), "{case}");
    }
}

#[test]
fn int_is_python_int_of_a_str_with_its_value_error() {
    let oracle = oracle();
    for case in cases(&oracle, "int10") {
        assert_eq!(
            int(text(&case[0])).map(|value| value.to_string()),
            expected(&case[1]),
            "{case}"
        );
    }
}

#[test]
fn an_int_beyond_64_bits_is_an_error_python_would_not_raise() {
    // Documented divergence: Python's int has no bound; the Rust fields are i64.
    let error = int("9223372036854775808").unwrap_err();
    assert_eq!(
        error,
        "int() value out of the 64-bit range: '9223372036854775808'"
    );
    assert_eq!(int("-9223372036854775808"), Ok(i64::MIN));
}

#[test]
fn repr_str_is_python_repr() {
    let oracle = oracle();
    for case in cases(&oracle, "repr") {
        assert_eq!(repr_str(text(&case[0])), text(&case[1]), "{case}");
    }
}

#[test]
fn fromhex_is_bytes_fromhex() {
    let oracle = oracle();
    for case in cases(&oracle, "fromhex") {
        assert_eq!(
            fromhex(text(&case[0])).map(hex::encode),
            expected(&case[1]),
            "{case}"
        );
    }
}

#[test]
fn b64decode_is_base64_b64decode_in_both_modes() {
    let oracle = oracle();
    for case in cases(&oracle, "b64decode") {
        let validate = case[1].as_bool().unwrap();
        assert_eq!(
            b64decode(text(&case[0]), validate).map(hex::encode),
            expected(&case[2]),
            "{case}"
        );
    }
}

#[test]
fn decode_utf16le_is_the_strict_codec_with_its_messages() {
    let oracle = oracle();
    for case in cases(&oracle, "utf16le") {
        let data = hex::decode(text(&case[0])).unwrap();
        assert_eq!(decode_utf16le(&data), expected(&case[1]), "{case}");
    }
}

#[test]
fn pure_posix_path_is_pathlib() {
    let oracle = oracle();
    for case in cases(&oracle, "purepath") {
        let path = PurePosixPath::new(text(&case[0]));
        let expected = &case[1];
        assert_eq!(path.to_string(), text(&expected["str"]), "{case}");
        assert_eq!(path.name(), text(&expected["name"]), "{case}");
        assert_eq!(
            path.parent().to_string(),
            text(&expected["parent"]),
            "{case}"
        );
        let parts: Vec<&str> = expected["parts"]
            .as_array()
            .unwrap()
            .iter()
            .map(text)
            .collect();
        assert_eq!(path.parts(), parts, "{case}");
        assert_eq!(path.stem(), text(&expected["stem"]), "{case}");
        assert_eq!(path.suffix(), text(&expected["suffix"]), "{case}");
        assert_eq!(
            path.is_absolute(),
            expected["is_absolute"].as_bool().unwrap(),
            "{case}"
        );
        assert_eq!(path.repr(), text(&expected["repr"]), "{case}");
    }
    for case in cases(&oracle, "join") {
        assert_eq!(
            PurePosixPath::new(text(&case[0]))
                .join(text(&case[1]))
                .to_string(),
            text(&case[2]),
            "{case}"
        );
    }
}

#[test]
fn urlparse_and_urljoin_are_urllib_parse() {
    let oracle = oracle();
    for case in cases(&oracle, "urlparse") {
        let outcome = &case[1];
        match urlparse(text(&case[0]), "") {
            Ok(parsed) => {
                let expected: Vec<&str> = outcome["ok"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{case}"))
                    .iter()
                    .map(text)
                    .collect();
                assert_eq!(
                    [
                        parsed.scheme.as_str(),
                        &parsed.netloc,
                        &parsed.path,
                        &parsed.params,
                        &parsed.query,
                        &parsed.fragment
                    ],
                    expected.as_slice(),
                    "{case}"
                );
            }
            Err(message) => assert_eq!(message, text(&outcome["error"]), "{case}"),
        }
    }
    for case in cases(&oracle, "urljoin") {
        assert_eq!(
            urljoin(text(&case[0]), text(&case[1])),
            expected(&case[2]),
            "{case}"
        );
    }
}

#[test]
fn fnmatch_is_the_translated_pattern_fullmatched() {
    let oracle = oracle();
    let names: Vec<&str> = [
        "seg_1.m4s",
        "seg_10.m4s",
        "seg_.m4s",
        "a",
        "b",
        "c",
        "d",
        "e",
        "z",
        "-",
        "]",
        "^",
        "&",
        "~",
        "|",
        "\\",
        "[",
        "ab",
        "",
        "a\nb",
        "aXb",
        "aXbYc",
        "acb",
        "xyz",
        "*",
        "seg_1.m4s.bak",
        "a]",
        "[a",
        "abcd",
        "abYce",
        "abYcf",
        "!",
        "out_1.m4s",
        "x.m4s",
        "\u{e9}",
        "aab",
        "[]",
    ]
    .to_vec();
    for case in cases(&oracle, "fnmatch") {
        let pattern = text(&case[0]);
        let expected: Vec<&str> = case[1].as_array().unwrap().iter().map(text).collect();
        let matched: Vec<&str> = names
            .iter()
            .copied()
            .filter(|name| fnmatch(name, pattern))
            .collect();
        assert_eq!(matched, expected, "pattern {pattern:?}");
    }
}

#[test]
fn repr_int_list_is_a_list_repr() {
    let oracle = oracle();
    for case in cases(&oracle, "int_list_repr") {
        let values: Vec<i64> = case[0]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_i64().unwrap())
            .collect();
        assert_eq!(repr_int_list(&values), text(&case[1]), "{case}");
    }
}

fn names(paths: &[PathBuf]) -> Vec<String> {
    let mut names: Vec<String> = paths
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn glob_is_path_glob_for_one_component() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for name in [
        "seg_0.m4s",
        "seg_1.m4s",
        "seg_10.m4s",
        ".seg_2.m4s",
        "init.mp4",
    ] {
        std::fs::write(root.join(name), b"x").unwrap();
    }
    std::fs::create_dir(root.join("seg_9.m4s")).unwrap();
    std::fs::create_dir_all(root.join("sub/deeper")).unwrap();
    // Wildcards: every kind of entry, hidden ones too (pathlib's `*` matches a leading dot).
    assert_eq!(
        names(&glob(root, "seg_*.m4s").unwrap()),
        ["seg_0.m4s", "seg_1.m4s", "seg_10.m4s", "seg_9.m4s"]
    );
    assert_eq!(names(&glob(root, "*seg_*").unwrap()).len(), 5);
    // Precise: the path when it exists, nothing otherwise.
    assert_eq!(names(&glob(root, "init.mp4").unwrap()), ["init.mp4"]);
    assert_eq!(glob(root, "absent.mp4").unwrap(), Vec::<PathBuf>::new());
    assert_eq!(glob(root, "..").unwrap(), [root.join("..")]);
    // A directory that does not exist yields nothing.
    assert_eq!(
        glob(&root.join("missing"), "*").unwrap(),
        Vec::<PathBuf>::new()
    );
    // `**` alone: the directory and every directory below it.
    let mut all = glob(root, "**").unwrap();
    all.sort();
    assert_eq!(
        all,
        [
            root.to_path_buf(),
            root.join("seg_9.m4s"),
            root.join("sub"),
            root.join("sub/deeper")
        ]
    );
    // pathlib's errors.
    assert_eq!(
        glob(root, "").unwrap_err().to_string(),
        "Unacceptable pattern: ''"
    );
    assert_eq!(
        glob(root, "seg_**.m4s").unwrap_err().to_string(),
        "Invalid pattern: '**' can only be an entire path component"
    );
}

#[test]
fn resolve_is_path_resolve_non_strict() {
    let dir = tempfile::tempdir().unwrap();
    // macOS puts temporary directories behind the /var -> /private/var link.
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::create_dir_all(root.join("real/inner")).unwrap();
    std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
    std::os::unix::fs::symlink("real/inner", root.join("rel")).unwrap();
    std::os::unix::fs::symlink("loop2", root.join("loop1")).unwrap();
    std::os::unix::fs::symlink("loop1", root.join("loop2")).unwrap();

    let resolved = |path: &Path| resolve(path).unwrap();
    assert_eq!(resolved(&root.join("link/inner")), root.join("real/inner"));
    assert_eq!(resolved(&root.join("rel/../x")), root.join("real/x"));
    assert_eq!(
        resolved(&root.join("link/missing/../y")),
        root.join("real/y"),
        "a missing component is kept lexically and `..` pops it"
    );
    assert_eq!(resolved(&root.join("./a//b/.")), root.join("a/b"));
    assert_eq!(resolved(Path::new("/../..")), PathBuf::from("/"));
    // Python 3.11's non-strict resolve still raises RuntimeError on a symlink loop.
    let looped = root.join("loop1/x");
    assert_eq!(
        resolve(&looped).unwrap_err().to_string(),
        format!("Symlink loop from {}", repr_str(&looped.to_string_lossy()))
    );
    // A relative path resolves against the working directory.
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(
        resolved(Path::new("no-such-dir-0123/f")),
        cwd.join("no-such-dir-0123/f")
    );
}

/// A parsed root as the reference records it: the tag and attribute names in Clark notation, the
/// text (`None` as `""`).
fn etree_json(document: &roxmltree::Document<'_>) -> Value {
    let root = document.root_element();
    let clark = |namespace: Option<&str>, name: &str| match namespace {
        Some(namespace) => format!("{{{namespace}}}{name}"),
        None => name.to_string(),
    };
    let attrib: serde_json::Map<String, Value> = root
        .attributes()
        .map(|attribute| {
            (
                clark(attribute.namespace(), attribute.name()),
                Value::String(attribute.value().to_string()),
            )
        })
        .collect();
    json!({
        "tag": clark(root.tag_name().namespace(), root.tag_name().name()),
        "text": etree_text(root),
        "attrib": attrib,
    })
}

#[test]
fn etree_parse_matches_expat() {
    // Character references, line ends, namespace declarations, processing instructions, the
    // XML declaration, content after the root and every truncation of a small document: the
    // tree or the `ParseError` text expat gives.
    let oracle = oracle();
    let mut differences = Vec::new();
    for case in cases(&oracle, "etree") {
        let text = xml_line_ends(text(&case[0]));
        let actual = etree_parse(&text).map(|document| etree_json(&document));
        let expected = match case[1].get("ok") {
            Some(tree) => {
                let mut tree = tree.clone();
                if tree["text"].is_null() {
                    tree["text"] = json!("");
                }
                Ok(tree)
            }
            None => Err(case[1]["error"].as_str().unwrap().to_string()),
        };
        if actual != expected {
            differences.push(format!("{}: {actual:?} != {expected:?}", case[0]));
        }
    }
    assert_eq!(differences, Vec::<String>::new());
}

#[test]
fn etree_get_is_the_attribute_without_a_namespace() {
    // `Element.get(name)`: a namespaced attribute never stands in for the plain one.
    let text = "<a xmlns:x='urn:x' x:b='1' xml:lang='en' c='2' x:c='3'/>";
    let document = etree_parse(text).unwrap();
    let root = document.root_element();
    assert_eq!(etree_get(root, "b"), None);
    assert_eq!(etree_get(root, "lang"), None);
    assert_eq!(etree_get(root, "c"), Some("2"));
}

fn nested(depth: usize) -> String {
    format!("{}{}", "<a>".repeat(depth), "</a>".repeat(depth))
}

#[test]
fn deep_nesting_parses_on_any_caller_stack_as_expat_does() {
    // The Rust parser recurses once per level; expat does not. The parse runs on a stack sized
    // for the document, so even a small caller stack takes the deepest supported document.
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let text = nested(MAX_XML_DEPTH);
            etree_parse(&text).map(|document| document.descendants().count())
        })
        .unwrap();
    assert_eq!(handle.join().unwrap(), Ok(MAX_XML_DEPTH + 1));
}

#[test]
fn nesting_beyond_the_supported_depth_is_an_error_not_an_abort() {
    // Documented limit: expat has none; here a document nested deeper than MAX_XML_DEPTH is
    // refused before it could exhaust any stack.
    assert_eq!(MAX_XML_DEPTH, 20_000);
    let error = etree_parse(&nested(MAX_XML_DEPTH + 1)).unwrap_err();
    assert_eq!(
        error,
        "nesting deeper than 20000 elements is not supported: line 1, column 60000"
    );
}

#[test]
fn xml_line_ends_are_normalized_before_parsing() {
    assert_eq!(xml_line_ends("a\r\nb\rc\n"), "a\nb\nc\n");
    assert!(matches!(
        xml_line_ends("a\nb"),
        std::borrow::Cow::Borrowed(_)
    ));
}
