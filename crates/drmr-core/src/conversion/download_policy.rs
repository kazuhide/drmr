//! Strict private network settings for explicitly requested remote conversion input.
use super::ConversionError;
use crate::source::FetchPolicy;
use serde::Deserialize;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::path::Path;

const INVALID: ConversionError = ConversionError("fetch-policy-invalid");
const LIMIT: u64 = 65_536;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    allowed_origins: Vec<String>,
    headers_by_origin: BTreeMap<String, BTreeMap<String, String>>,
    allow_http: bool,
    allow_private_network: bool,
    max_playlist_bytes: Option<u64>,
    max_playlists: Option<u64>,
    max_segments_per_track: Option<u64>,
    max_resource_bytes: Option<u64>,
    max_total_bytes: Option<u64>,
    min_free_disk_bytes: Option<u64>,
    timeout: Option<f64>,
    attempts: Option<u32>,
    connections: Option<usize>,
    max_total_resources: Option<u64>,
}

fn origin(url: &str) -> Result<String, ConversionError> {
    let origin = crate::source::origin(url).map_err(|_| INVALID)?;
    if !origin.starts_with("https://") && !origin.starts_with("http://") {
        return Err(INVALID);
    }
    Ok(origin)
}

/// The requested origin is always allowed; the optional file grants additional access.
/// No request is made, and neither parsing errors nor debug output expose credentials.
pub fn read(url: &str, path: Option<&Path>) -> Result<FetchPolicy, ConversionError> {
    let initial = origin(url)?;
    let settings = match path {
        None => Settings::default(),
        Some(path) => {
            let file = crate::native::open_private_read(path).map_err(|_| INVALID)?;
            if file.metadata().map_err(|_| INVALID)?.len() > LIMIT {
                return Err(INVALID);
            }
            let mut bytes = Vec::new();
            file.take(LIMIT + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| INVALID)?;
            if bytes.len() as u64 > LIMIT {
                return Err(INVALID);
            }
            let value = serde_json::from_slice::<UniqueValue>(&bytes).map_err(|_| INVALID)?;
            if value
                .0
                .as_object()
                .is_some_and(|settings| settings.values().any(Value::is_null))
            {
                return Err(INVALID);
            }
            serde_json::from_value::<Settings>(value.0).map_err(|_| INVALID)?
        }
    };
    let mut policy = FetchPolicy {
        allowed_origins: vec![initial],
        allow_http: settings.allow_http,
        allow_private_network: settings.allow_private_network,
        ..FetchPolicy::default()
    };
    for extra in settings.allowed_origins {
        if origin(&extra)? != extra {
            return Err(INVALID);
        }
        if !policy.allowed_origins.contains(&extra) {
            policy.allowed_origins.push(extra);
        }
    }
    policy.headers_by_origin = settings
        .headers_by_origin
        .into_iter()
        .map(|(origin, headers)| (origin, headers.into_iter().collect()))
        .collect();
    macro_rules! assign {
        ($($field:ident),+ $(,)?) => {$(
            if let Some(value) = settings.$field { policy.$field = value; }
        )+};
    }
    assign!(
        max_playlist_bytes,
        max_playlists,
        max_segments_per_track,
        max_resource_bytes,
        max_total_bytes,
        min_free_disk_bytes,
        timeout,
        attempts,
        connections
    );
    policy.max_total_resources = settings.max_total_resources;
    policy.validate().map_err(|_| INVALID)?;
    Ok(policy)
}

struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = UniqueValue;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("JSON without duplicate object members")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut result = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if result.contains_key(&key) {
                        return Err(de::Error::custom("duplicate object member"));
                    }
                    result.insert(key, map.next_value::<UniqueValue>()?.0);
                }
                Ok(UniqueValue(Value::Object(result)))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut result = Vec::new();
                while let Some(value) = seq.next_element::<UniqueValue>()? {
                    result.push(value.0);
                }
                Ok(UniqueValue(Value::Array(result)))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::String(value.into())))
            }
            fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::String(value)))
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Bool(value)))
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Number(value.into())))
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Number(value.into())))
            }
            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
                Number::from_f64(value)
                    .map(|n| UniqueValue(Value::Number(n)))
                    .ok_or_else(|| de::Error::custom("nonfinite number"))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }
        }
        deserializer.deserialize_any(UniqueVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn defaults_allow_only_the_explicit_origin_without_weakening_network_rules() {
        let policy = read("https://example.test:443/input.m3u8?token=private", None).unwrap();
        assert_eq!(policy.allowed_origins, ["https://example.test"]);
        assert!(!policy.allow_http && !policy.allow_private_network);
        assert!(policy.allowed_local_roots.is_empty() && policy.headers_by_origin.is_empty());
        for url in [
            "file:///input.m3u8",
            "https://user:secret@example.test/input",
            "https://example.test/input#fragment",
        ] {
            assert_eq!(read(url, None).unwrap_err(), INVALID);
        }
    }

    #[test]
    fn private_policy_is_typed_and_rejects_duplicates_at_every_depth() {
        let root = crate::native::private_tempdir().unwrap();
        let path = root.path().join("fetch.json");
        for body in [
            r#"{"allow_http":"true"}"#,
            r#"{"allow_http":true,"allow_http":false}"#,
            r#"{"headers_by_origin":{"https://example.test":{"Authorization":"first","Authorization":"second"}}}"#,
            r#"{"allowed_origins":"https://example.test"}"#,
            r#"{"allowed_local_roots":["/"]}"#,
            r#"{"connections":0}"#,
            r#"{"connections":1.0}"#,
            r#"{"connections":null}"#,
            r#"{"timeout":null}"#,
            r#"{"attempts":null}"#,
            r#"{"max_total_bytes":null}"#,
            r#"{"max_total_bytes":true}"#,
            r#"{"allowed_origins":["https://example.test/path"]}"#,
            r#"{"headers_by_origin":{"https://elsewhere.test":{"Authorization":"secret"}}}"#,
            r#"{"timeout":-1}"#,
        ] {
            crate::native::create_private_or_truncate(&path)
                .unwrap()
                .write_all(body.as_bytes())
                .unwrap();
            assert_eq!(
                read("https://example.test/input", Some(&path)).unwrap_err(),
                INVALID
            );
        }
        crate::native::create_private_or_truncate(&path).unwrap().write_all(
            br#"{"allowed_origins":["https://cdn.example.test"],"headers_by_origin":{"https://example.test":{"Authorization":"secret"}},"max_total_bytes":4096,"connections":2}"#
        ).unwrap();
        let policy = read("https://example.test/input", Some(&path)).unwrap();
        assert_eq!(
            policy.allowed_origins,
            ["https://example.test", "https://cdn.example.test"]
        );
        assert_eq!(policy.max_total_bytes, 4096);
        assert_eq!(policy.connections, 2);
        assert_eq!(
            policy.headers_for("https://example.test"),
            [("Authorization".into(), "secret".into())]
        );
        assert_eq!(
            policy.headers_for("https://cdn.example.test"),
            Vec::<(String, String)>::new()
        );
    }
}
