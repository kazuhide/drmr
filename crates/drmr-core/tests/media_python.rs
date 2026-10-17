//! The Python semantics of the media helpers (`media::python`) against what `CPython` 3.11
//! printed for the same inputs (`fixtures/media/`literals`thon311.json`,
//! `fixtures/media/`pure`thon311.json`; synthetic inputs only).

use drmr_core::canonical::python_float_repr;
use drmr_core::media::MediaError;
use drmr_core::media::python::{
    self, eq_int, fraction, fraction_to_f64, fromhex, path_name, path_str, py_eq, py_str, repr_str,
    resolve, splitlines, true_divide, truthy,
};
use num_bigint::BigInt;
use num_rational::BigRational;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/media")
        .join(name);
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap()
}

/// The class and message of an error, as the fixture records Python's.
fn class_and_message(error: &MediaError) -> (String, String) {
    (error.python_class().to_string(), error.to_string())
}

#[test]
fn float_parses_literals_as_cpython_does() {
    let literals = fixture("literals.python311.json");
    for case in literals["float"].as_array().unwrap() {
        let input = text(&case[0]);
        let result = python::float(&json!(input));
        match text(&case[1]) {
            "ok" => {
                let value = result.unwrap_or_else(|e| panic!("{input:?}: {e}"));
                match text(&case[2]) {
                    "nan" => assert!(value.is_nan(), "{input:?}"),
                    "inf" => assert!(value.is_infinite() && value > 0.0, "{input:?}"),
                    "-inf" => assert!(value.is_infinite() && value < 0.0, "{input:?}"),
                    expected => assert_eq!(python_float_repr(value), expected, "{input:?}"),
                }
            }
            class => {
                let error = result.expect_err(input);
                assert_eq!(
                    class_and_message(&error),
                    (class.to_string(), text(&case[2]).to_string()),
                    "{input:?}"
                );
            }
        }
    }
}

#[test]
fn int_parses_literals_as_cpython_does() {
    let literals = fixture("literals.python311.json");
    for case in literals["int"].as_array().unwrap() {
        let input = text(&case[0]);
        let result = python::int(&json!(input));
        if text(&case[1]) == "ok" {
            assert_eq!(result.unwrap().to_string(), text(&case[2]), "{input:?}");
        } else {
            assert_eq!(
                class_and_message(&result.expect_err(input)),
                (text(&case[1]).to_string(), text(&case[2]).to_string()),
                "{input:?}"
            );
        }
    }
    // Numbers, booleans and the types int refuses.
    assert_eq!(python::int(&json!(7.9)).unwrap(), 7);
    assert_eq!(python::int(&json!(-7.9)).unwrap(), -7);
    assert_eq!(python::int(&json!(true)).unwrap(), 1);
    assert_eq!(python::int(&json!(u64::MAX)).unwrap(), i128::from(u64::MAX));
    let refused = python::int(&Value::Null).unwrap_err();
    assert_eq!(refused.python_class(), "TypeError");
    assert_eq!(
        refused.to_string(),
        "int() argument must be a string, a bytes-like object or a real number, not 'NoneType'"
    );
    assert_eq!(
        python::int(&json!([1])).unwrap_err().python_class(),
        "TypeError"
    );
    assert_eq!(python::int64(&json!(i64::MAX)).unwrap(), i64::MAX);
    assert_eq!(
        python::int64(&json!(u64::MAX)).unwrap_err().python_class(),
        "OverflowError",
        "beyond int64_t: a narrowing ffprobe's fields never reach"
    );
}

#[test]
fn fraction_parses_literals_as_cpython_311_does() {
    let literals = fixture("literals.python311.json");
    for case in literals["fraction"].as_array().unwrap() {
        let input = text(&case[0]);
        let result = fraction(&json!(input));
        if text(&case[1]) == "ok" {
            let value = result.unwrap_or_else(|e| panic!("{input:?}: {e}"));
            assert_eq!(
                (value.numer().to_string(), value.denom().to_string()),
                (text(&case[2]).to_string(), text(&case[3]).to_string()),
                "{input:?}"
            );
        } else {
            assert_eq!(
                class_and_message(&result.expect_err(input)),
                (text(&case[1]).to_string(), text(&case[2]).to_string()),
                "{input:?}"
            );
        }
    }
    assert_eq!(
        fraction(&json!(3)).unwrap(),
        BigRational::from_integer(3.into())
    );
    assert_eq!(
        fraction(&json!(0.5)).unwrap(),
        BigRational::new(1.into(), 2.into()),
        "a float exactly"
    );
    assert_eq!(
        fraction(&Value::Null).unwrap_err().to_string(),
        "argument should be a string or a Rational instance"
    );
}

#[test]
fn splitlines_and_fromhex_match_cpython() {
    let literals = fixture("literals.python311.json");
    for case in literals["splitlines"].as_array().unwrap() {
        let expected: Vec<&str> = case[1].as_array().unwrap().iter().map(text).collect();
        assert_eq!(splitlines(text(&case[0])), expected, "{:?}", case[0]);
    }
    for case in literals["fromhex"].as_array().unwrap() {
        let input = text(&case[0]);
        let result = fromhex(input);
        if text(&case[1]) == "ok" {
            assert_eq!(hex::encode(&*result.unwrap()), text(&case[2]), "{input:?}");
        } else {
            assert_eq!(
                class_and_message(&result.expect_err(input)),
                (text(&case[1]).to_string(), text(&case[2]).to_string()),
                "{input:?}"
            );
        }
    }
}

#[test]
fn true_divide_is_cpythons_correctly_rounded_quotient() {
    let pure = fixture("pure.python311.json");
    let cases = pure["true_divide"].as_array().unwrap();
    assert!(cases.len() > 400);
    for case in cases {
        let a: BigInt = text(&case[0]).parse().unwrap();
        let b: BigInt = text(&case[1]).parse().unwrap();
        let result = true_divide(&a, &b);
        if text(&case[2]) == "ok" {
            let value = result.unwrap_or_else(|e| panic!("{a} / {b}: {e}"));
            assert_eq!(python_float_repr(value), text(&case[3]), "{a} / {b}");
        } else {
            assert_eq!(
                class_and_message(&result.expect_err("an error")),
                (text(&case[2]).to_string(), text(&case[3]).to_string()),
                "{a} / {b}"
            );
        }
    }
    let error = true_divide(&BigInt::from(1), &BigInt::from(0)).unwrap_err();
    assert_eq!(
        (error.python_class(), error.to_string().as_str()),
        ("ZeroDivisionError", "division by zero")
    );
}

#[test]
fn a_fraction_prints_as_str_of_its_float() {
    let pure = fixture("pure.python311.json");
    for case in pure["offset_text"].as_array().unwrap() {
        let value = BigRational::new(
            text(&case[0]).parse::<BigInt>().unwrap(),
            text(&case[1]).parse::<BigInt>().unwrap(),
        );
        assert_eq!(
            python_float_repr(fraction_to_f64(&value).unwrap()),
            text(&case[2]),
            "{value}"
        );
    }
}

#[test]
fn path_text_and_name_are_pathlibs() {
    let pure = fixture("pure.python311.json");
    for case in pure["paths"].as_array().unwrap() {
        let path = Path::new(text(&case[0]));
        assert_eq!(path_str(path).unwrap(), text(&case[1]), "{path:?}");
        assert_eq!(path_name(path).unwrap(), text(&case[2]), "{path:?}");
    }
}

#[cfg(unix)]
#[test]
fn resolve_follows_symlinks_like_cpython_311() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join("real")).unwrap();
    symlink("real", root.join("link")).unwrap();
    symlink(root.join("real"), root.join("abs-link")).unwrap();
    symlink("b", root.join("a")).unwrap();
    symlink("a", root.join("b")).unwrap();
    symlink("../real/../real", root.join("real/up")).unwrap();
    let canonical = path_str(&std::fs::canonicalize(root).unwrap()).unwrap();
    // The values CPython 3.11 printed for the same layout (<root> its resolved directory).
    for (case, expected) in [
        ("link/x/../y", "<root>/real/y"),
        ("abs-link/f.key", "<root>/real/f.key"),
        ("real/up/z", "<root>/real/z"),
        ("missing/../real/./w", "<root>/real/w"),
        ("link/..", "<root>"),
        ("./link", "<root>/real"),
    ] {
        let resolved = resolve(&root.join(case)).unwrap();
        assert_eq!(
            path_str(&resolved).unwrap(),
            expected.replace("<root>", &canonical),
            "{case}"
        );
    }
    let looped = resolve(&root.join("a/x")).unwrap_err();
    assert!(looped.is_runtime_error(), "Python raises RuntimeError");
    assert_eq!(
        looped.to_string(),
        format!("Symlink loop from '{canonical}/a/x'")
    );
}

#[test]
fn truth_values_and_equality_are_pythons() {
    for falsy in [
        json!(null),
        json!(false),
        json!(0),
        json!(0.0),
        json!(""),
        json!([]),
        json!({}),
    ] {
        assert!(!truthy(&falsy), "{falsy}");
    }
    for value in [
        json!("[]"),
        json!(1),
        json!(-0.5),
        json!([0]),
        json!({"a": 0}),
    ] {
        assert!(truthy(&value), "{value}");
    }
    assert!(eq_int(&json!(0), 0) && eq_int(&json!(0.0), 0) && eq_int(&json!(false), 0));
    assert!(!eq_int(&json!("0"), 0) && !eq_int(&json!(null), 0) && !eq_int(&json!(0.5), 0));
    assert!(!eq_int(
        &json!(9_007_199_254_740_993_u64),
        9_007_199_254_740_992
    ));
    assert!(py_eq(&json!(281), &json!(281.0)));
    assert!(py_eq(&json!(true), &json!(1)));
    assert!(py_eq(
        &json!({"a": 1, "b": [1, 2.0]}),
        &json!({"b": [1.0, 2], "a": true})
    ));
    assert!(!py_eq(&json!("1"), &json!(1)));
    assert!(!py_eq(
        &json!(9_007_199_254_740_993_u64),
        &json!(9_007_199_254_740_992.0)
    ));
    assert_eq!(py_str(&json!(281)), "281");
    assert_eq!(py_str(&json!(281.0)), "281.0");
    assert_eq!(py_str(&json!(true)), "True");
    assert_eq!(py_str(&json!(null)), "None");
    assert_eq!(py_str(&json!("x")), "x");
    assert_eq!(py_str(&json!([1, "a", null])), "[1, 'a', None]");
    assert_eq!(repr_str("wav"), "'wav'");
    assert_eq!(repr_str("it's"), "\"it's\"");
    assert_eq!(repr_str("a\\b\n\u{1c}"), "'a\\\\b\\n\\x1c'");
}
