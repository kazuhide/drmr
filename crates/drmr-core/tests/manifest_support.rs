//! Standard-library compatibility for XML, fractions, formatting, text, and paths.

use drmr_core::manifest_support::fraction::Fraction;
use drmr_core::manifest_support::py::{
    self, PyErr, decode_utf8, float_str, format_fixed, int_of_float, isdigit, parse_float,
    parse_int, repr_str, sorted_indices, splitlines, strip,
};
use drmr_core::manifest_support::xml::{self, Element};
use drmr_core::manifest_support::{path, py::PyOsError};
use num_bigint::BigInt;
use serde_json::{Value, json};
use std::path::PathBuf;

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/manifests")
        .join(name);
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn tree(element: &Element) -> Value {
    json!({
        "tag": element.tag,
        "attrib": element.attrib.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
        "text": element.text,
        "tail": element.tail,
        "children": element.children.iter().map(tree).collect::<Vec<_>>(),
    })
}

/// `{"ok": …}` / `{"error": [class, message]}` of a Rust result.
fn outcome<T: Into<Value>>(result: Result<T, PyErr>) -> Value {
    match result {
        Ok(value) => json!({ "ok": value.into() }),
        Err(err) => json!({ "error": [err.class_name(), err.to_string()] }),
    }
}

fn str_of(value: &Value) -> &str {
    value.as_str().unwrap()
}

#[test]
fn elementtree_fromstring_matches_expat_on_the_corpus() {
    let corpus = fixture("xml-corpus.json");
    let rows = corpus.as_array().unwrap();
    let errors = rows.iter().filter(|row| row.get("error").is_some()).count();
    assert!(
        rows.len() > 200 && errors > 150,
        "{} rows, {errors} errors",
        rows.len()
    );
    let mut mismatches = Vec::new();
    for case in rows {
        let input = str_of(&case["input"]);
        let actual = match xml::fromstring(input) {
            Ok(root) => json!({ "input": input, "tree": tree(&root) }),
            Err(err) => json!({ "input": input, "error": err.to_string() }),
        };
        if &actual != case {
            mismatches.push(format!("{input:?}\n  python: {case}\n  rust:   {actual}"));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn int_and_float_parse_like_python() {
    let semantics = fixture("py-semantics.json");
    for case in semantics["int"].as_array().unwrap() {
        let input = str_of(&case[0]);
        let actual = outcome(parse_int(input).map(|value| value.to_string()));
        assert_eq!(actual, case[1], "int({input:?})");
    }
    for case in semantics["float"].as_array().unwrap() {
        let input = str_of(&case[0]);
        let actual = outcome(parse_float(input).map(float_str));
        assert_eq!(actual, case[1], "float({input:?})");
    }
}

#[test]
fn str_methods_match_python() {
    let semantics = fixture("py-semantics.json");
    for case in semantics["repr"].as_array().unwrap() {
        assert_eq!(repr_str(str_of(&case[0])), str_of(&case[1]));
    }
    for case in semantics["splitlines"].as_array().unwrap() {
        let expected: Vec<&str> = case[1].as_array().unwrap().iter().map(str_of).collect();
        assert_eq!(splitlines(str_of(&case[0])), expected, "{:?}", case[0]);
    }
    for case in semantics["isdigit"].as_array().unwrap() {
        assert_eq!(
            isdigit(str_of(&case[0])),
            case[1].as_bool().unwrap(),
            "{:?}",
            case[0]
        );
    }
    for case in semantics["strip"].as_array().unwrap() {
        assert_eq!(strip(str_of(&case[0])), str_of(&case[1]), "{:?}", case[0]);
    }
}

#[test]
fn fractions_match_python() {
    let semantics = fixture("py-semantics.json");
    for case in semantics["fraction"].as_array().unwrap() {
        let input = str_of(&case[0]);
        let actual = outcome(
            Fraction::parse(input)
                .map(|f| json!([f.numerator().to_string(), f.denominator().to_string()])),
        );
        assert_eq!(actual, case[1], "Fraction({input:?})");
    }
    for case in semantics["fraction_float"].as_array().unwrap() {
        let numerator: BigInt = str_of(&case[0][0]).parse().unwrap();
        let denominator: BigInt = str_of(&case[0][1]).parse().unwrap();
        let fraction = Fraction::new(numerator, denominator).unwrap();
        let actual = outcome(fraction.to_f64().map(float_str));
        assert_eq!(actual, case[1], "float(Fraction({:?}))", case[0]);
    }
    for case in semantics["fraction_round"].as_array().unwrap() {
        let numerator: BigInt = str_of(&case[0][0]).parse().unwrap();
        let denominator: BigInt = str_of(&case[0][1]).parse().unwrap();
        let fraction = Fraction::new(numerator, denominator).unwrap();
        let actual = json!([
            fraction.trunc().to_string(),
            fraction.floor().to_string(),
            fraction.ceil().to_string()
        ]);
        assert_eq!(actual, case[1], "rounding Fraction({:?})", case[0]);
    }
}

#[test]
fn float_formatting_and_conversion_match_python() {
    let semantics = fixture("py-semantics.json");
    for case in semantics["fixed"].as_array().unwrap() {
        let value = parse_float(str_of(&case[0])).unwrap();
        let actual = json!([
            format_fixed(value, 9),
            format_fixed(value, 6),
            format_fixed(value, 3),
            format_fixed(value, 0)
        ]);
        assert_eq!(actual, case[1], "{:?}", case[0]);
    }
    for case in semantics["int_of_float"].as_array().unwrap() {
        let value = parse_float(str_of(&case[0])).unwrap();
        let actual = outcome(int_of_float(value).map(|value| value.to_string()));
        assert_eq!(actual, case[1], "int({:?})", case[0]);
    }
    for case in semantics["float_str"].as_array().unwrap() {
        let value = parse_float(str_of(&case[0])).unwrap();
        assert_eq!(float_str(value), str_of(&case[1]));
    }
}

#[test]
fn sorting_float_keys_matches_python_311() {
    let semantics = fixture("py-semantics.json");
    for case in semantics["sorted"].as_array().unwrap() {
        let values: Vec<f64> = case[0]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| parse_float(str_of(value)).unwrap())
            .collect();
        let expected: Vec<usize> = case[1]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| usize::try_from(value.as_u64().unwrap()).unwrap())
            .collect();
        assert_eq!(sorted_indices(&values), expected, "{:?}", case[0]);
    }
}

#[test]
fn utf8_decode_errors_match_python() {
    let semantics = fixture("py-semantics.json");
    for case in semantics["utf8"].as_array().unwrap() {
        let bytes = hex::decode(str_of(&case[0])).unwrap();
        assert_eq!(outcome(decode_utf8(bytes)), case[1], "{:?}", case[0]);
    }
}

#[test]
fn pure_path_names_match_python() {
    let semantics = fixture("py-semantics.json");
    for case in semantics["path"].as_array().unwrap() {
        let input = str_of(&case[0]);
        let actual = json!([
            path::pure(input),
            path::name(input),
            path::stem(input),
            path::parent(input)
        ]);
        assert_eq!(actual, case[1], "{input:?}");
    }
}

#[test]
fn os_errors_print_like_python() {
    let missing = std::env::temp_dir().join("drmr-manifest-support-missing-file");
    let err = py::read_bytes(&missing).unwrap_err();
    let PyErr::Os(PyOsError {
        message,
        kind,
        errno,
    }) = &err
    else {
        panic!("{err:?}");
    };
    assert_eq!(*kind, std::io::ErrorKind::NotFound);
    assert_eq!(*errno, Some(2));
    assert_eq!(
        message,
        &format!(
            "[Errno 2] No such file or directory: {}",
            repr_str(missing.to_str().unwrap())
        )
    );
}

#[test]
fn universal_newlines_translate_like_text_mode() {
    assert_eq!(py::universal_newlines("a\r\nb\rc\n\r"), "a\nb\nc\n\n");
    assert_eq!(py::universal_newlines("plain\n"), "plain\n");
}

#[test]
fn relpath_relative_to_and_resolve_follow_posixpath() {
    let dir = tempfile::tempdir().unwrap();
    let root = path::resolve(dir.path()).unwrap();
    let root_text = root.to_str().unwrap();
    // relpath across siblings and to the same directory (`_relative` of manifest_gen).
    let media = format!("{root_text}/cbcs/v000/seg_0.m4s");
    let manifests = format!("{root_text}/manifests");
    assert_eq!(
        path::relpath(&media, &manifests).unwrap(),
        "../cbcs/v000/seg_0.m4s"
    );
    assert_eq!(path::relpath(&manifests, &manifests).unwrap(), ".");
    assert_eq!(path::relative_to("/a/b/c", "/a").unwrap(), "b/c");
    assert_eq!(path::relative_to("/a/b", "/").unwrap(), "a/b");
    assert_eq!(path::relative_to("/a", "/a").unwrap(), ".");
    assert_eq!(
        path::relative_to("/a/b", "/c").unwrap_err().to_string(),
        "'/a/b' is not in the subpath of '/c' OR one path is relative and the other is absolute."
    );
    // `resolve` follows a symbolic link, then applies `..` (realpath order), and keeps a
    // missing tail as written.
    std::fs::create_dir_all(dir.path().join("real/inner")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("real/inner"), dir.path().join("link")).unwrap();
    let through = path::resolve(&dir.path().join("link/../missing/x")).unwrap();
    assert_eq!(through, root.join("real/missing/x"));
    assert_eq!(path::normpath("//a//b/../c/."), "//a/c");
    assert_eq!(path::normpath("///a/../.."), "/");
    assert_eq!(path::normpath("a/../../b"), "../b");
}
