//! Bounded configuration diagnostics made only from public schema tokens.
//!
//! Parser messages, operator identifiers, map keys and values never enter a diagnostic.
use super::ConversionError;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum File {
    Keys,
    Outputs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Field {
    SchemaVersion,
    Input,
    Keys,
    HlsKeys,
    Kid,
    Key,
    Uri,
    Iv,
    Output,
    Outputs,
    OutputDir,
    HlsKeyUri,
    Signaling,
    System,
    LicenseUrl,
    WidevinePssh,
    PlayreadyLicenseUrl,
    Fairplay,
    Id,
    Format,
    Target,
    OutputFormat,
    Encryption,
    Drms,
    Conformance,
    ExternalManifestCheck,
    ManifestProfile,
    Processing,
    DashAddressing,
    #[serde(rename = "*")]
    Entry,
}
impl Field {
    fn name(self) -> &'static str {
        match self {
            Self::SchemaVersion => "schema_version",
            Self::Input => "input",
            Self::Keys => "keys",
            Self::HlsKeys => "hls_keys",
            Self::Kid => "kid",
            Self::Key => "key",
            Self::Uri => "uri",
            Self::Iv => "iv",
            Self::Output => "output",
            Self::Outputs => "outputs",
            Self::OutputDir => "output_dir",
            Self::HlsKeyUri => "hls_key_uri",
            Self::Signaling => "signaling",
            Self::System => "system",
            Self::LicenseUrl => "license_url",
            Self::WidevinePssh => "widevine_pssh",
            Self::PlayreadyLicenseUrl => "playready_license_url",
            Self::Fairplay => "fairplay",
            Self::Id => "id",
            Self::Format => "format",
            Self::Target => "target",
            Self::OutputFormat => "output_format",
            Self::Encryption => "encryption",
            Self::Drms => "drms",
            Self::Conformance => "conformance",
            Self::ExternalManifestCheck => "external_manifest_check",
            Self::ManifestProfile => "manifest_profile",
            Self::Processing => "processing",
            Self::DashAddressing => "dash_addressing",
            Self::Entry => "*",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum Segment {
    Field(Field),
    Index(usize),
}
impl From<Field> for Segment {
    fn from(value: Field) -> Self {
        Self::Field(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Expected {
    KeysObject,
    OutputsObject,
    OutputDirectory,
    SchemaVersion,
    InputObject,
    InputKeys,
    HlsKeys,
    KeyObject,
    HlsKeyObject,
    OutputKey,
    OutputDomains,
    Signaling,
    #[serde(rename = "hex-16-bytes")]
    Hex16Bytes,
    SegmentIv,
    Uri,
    SignalingSystem,
    LicenseUrl,
    Pssh,
    Boolean,
    OutputsArray,
    OutputObject,
    OutputId,
    OutputFormat,
    TargetProtection,
    Drms,
    DrmSystem,
    Conformance,
    Processing,
    DashAddressing,
    ManifestProfile,
}
impl Expected {
    fn hint(self) -> &'static str {
        match self {
            Self::KeysObject => {
                "Use an owner-only regular key file of at most 64 KiB, with schema_version 2 and named outputs. Remove unknown or duplicate fields."
            }
            Self::OutputsObject => {
                "Use a readable regular output-configuration file of at most 64 KiB, with schema_version 4 and an outputs array. Remove unknown or duplicate fields."
            }
            Self::OutputDirectory => {
                "Use a nonempty local directory path without parent-directory components; relative paths refer to the configuration file directory."
            }
            Self::SchemaVersion => {
                "Use schema_version 2 for current key files or 4 for current output configurations."
            }
            Self::InputObject => "Use an input object containing only keys and hls_keys arrays.",
            Self::InputKeys => "Use an array of at most 64 keys with distinct key identifiers.",
            Self::HlsKeys => {
                "Use a bounded array with distinct HLS key URIs and associated IVs when required."
            }
            Self::KeyObject => {
                "Use a key object containing kid and key strings. Remove unknown or duplicate fields."
            }
            Self::HlsKeyObject => {
                "Use an HLS key object containing uri and key strings and, optionally, an iv string."
            }
            Self::OutputKey => {
                "Use an output key object with key and either kid or hls_key_uri. An iv cannot be combined with kid. Check the signaling fields."
            }
            Self::OutputDomains => {
                "Use an object with at most 16 distinct safe output IDs, each mapped to an output key object."
            }
            Self::Signaling => {
                "Use compatible playready or multi-drm signaling. Check its required fields, output key and HLS URI."
            }
            Self::Hex16Bytes => "Use a string of exactly 32 hexadecimal characters (16 bytes).",
            Self::SegmentIv => {
                "Use a hexadecimal string of 1 to 32 digits, optionally prefixed by 0x (at most 16 bytes)."
            }
            Self::Uri => {
                "Use a nonempty URI string of at most 4096 bytes without control characters. Output playlist URIs also refuse quotes; FairPlay requires skd://."
            }
            Self::SignalingSystem => "Use the string playready or multi-drm.",
            Self::LicenseUrl => "Use a valid bounded license URL for the selected DRM system.",
            Self::Pssh => {
                "Use a complete supported Widevine PSSH box in hexadecimal or Base64, matching the output key and encryption scheme."
            }
            Self::Boolean => "Use a JSON boolean: true or false.",
            Self::OutputsArray => {
                "Use an array of one to eight output objects with distinct safe IDs."
            }
            Self::OutputObject => {
                "Use a supported combination of output_format, encryption, DRM systems, processing and manifest checks."
            }
            Self::OutputId => {
                "Use a unique ID of 1 to 32 characters: a lowercase ASCII letter, followed by lowercase letters, digits or hyphens. Reserved device names are refused."
            }
            Self::OutputFormat => {
                "Use the string mp4, dash, hls-fmp4 or hls-ts in current output configurations."
            }
            Self::TargetProtection => {
                "Use supported encryption: clear, cenc, cbcs, hls-aes128 or hls-sample-aes."
            }
            Self::Drms => {
                "Use an array of at most three distinct DRM systems compatible with output_format and encryption; use [] for no DRM declarations."
            }
            Self::DrmSystem => "Use the string widevine, playready or fairplay.",
            Self::Conformance => {
                "Use none or required. External manifest checks are not supported for MP4 output."
            }
            Self::Processing => "Use auto, segments or stitch in the output configuration.",
            Self::DashAddressing => {
                "Use template, template-timeline, list or base for DASH outputs only."
            }
            Self::ManifestProfile => {
                "Use base for HLS or DASH, or apple-vod for HLS only. Manifest profiles are not supported for MP4 output."
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Node {
    KeysRoot,
    OutputsRoot,
    Input,
    Keys,
    Key,
    HlsKeys,
    HlsKey,
    Output,
    OutputDomains,
    Signaling,
    Outputs,
    Intent,
    Drms,
    Leaf(Expected),
}
impl Node {
    fn root(file: File) -> Self {
        match file {
            File::Keys => Self::KeysRoot,
            File::Outputs => Self::OutputsRoot,
        }
    }
    fn expected(self) -> Expected {
        match self {
            Self::KeysRoot => Expected::KeysObject,
            Self::OutputsRoot => Expected::OutputsObject,
            Self::Input => Expected::InputObject,
            Self::Keys => Expected::InputKeys,
            Self::Key => Expected::KeyObject,
            Self::HlsKeys => Expected::HlsKeys,
            Self::HlsKey => Expected::HlsKeyObject,
            Self::Output => Expected::OutputKey,
            Self::OutputDomains => Expected::OutputDomains,
            Self::Signaling => Expected::Signaling,
            Self::Outputs => Expected::OutputsArray,
            Self::Intent => Expected::OutputObject,
            Self::Drms => Expected::Drms,
            Self::Leaf(expected) => expected,
        }
    }
    fn field(self, field: Field) -> Option<Self> {
        use Field as F;
        Some(match (self, field) {
            (Self::KeysRoot | Self::OutputsRoot, F::SchemaVersion) => {
                Self::Leaf(Expected::SchemaVersion)
            }
            (Self::KeysRoot, F::Input) => Self::Input,
            (Self::KeysRoot, F::Output) | (Self::OutputDomains, F::Entry) => Self::Output,
            (Self::KeysRoot, F::Outputs) => Self::OutputDomains,
            (Self::Input, F::Keys) => Self::Keys,
            (Self::Input, F::HlsKeys) => Self::HlsKeys,
            (Self::Key | Self::Output, F::Kid | F::Key) | (Self::HlsKey, F::Key) => {
                Self::Leaf(Expected::Hex16Bytes)
            }
            (Self::HlsKey | Self::Output, F::Iv) => Self::Leaf(Expected::SegmentIv),
            (Self::HlsKey, F::Uri) | (Self::Output, F::HlsKeyUri) => Self::Leaf(Expected::Uri),
            (Self::Output, F::Signaling) => Self::Signaling,
            (Self::Signaling, F::System) => Self::Leaf(Expected::SignalingSystem),
            (Self::Signaling, F::LicenseUrl | F::PlayreadyLicenseUrl) => {
                Self::Leaf(Expected::LicenseUrl)
            }
            (Self::Signaling, F::WidevinePssh) => Self::Leaf(Expected::Pssh),
            (Self::Signaling, F::Fairplay) | (Self::Intent, F::ExternalManifestCheck) => {
                Self::Leaf(Expected::Boolean)
            }
            (Self::OutputsRoot, F::Outputs) => Self::Outputs,
            (Self::OutputsRoot, F::OutputDir) => Self::Leaf(Expected::OutputDirectory),
            (Self::Intent, F::Id) => Self::Leaf(Expected::OutputId),
            (Self::Intent, F::Format | F::OutputFormat) => Self::Leaf(Expected::OutputFormat),
            (Self::Intent, F::Target | F::Encryption) => Self::Leaf(Expected::TargetProtection),
            (Self::Intent, F::Drms) => Self::Drms,
            (Self::Intent, F::Conformance) => Self::Leaf(Expected::Conformance),
            (Self::Intent, F::ManifestProfile) => Self::Leaf(Expected::ManifestProfile),
            (Self::Intent, F::Processing) => Self::Leaf(Expected::Processing),
            (Self::Intent, F::DashAddressing) => Self::Leaf(Expected::DashAddressing),
            _ => return None,
        })
    }
    fn index(self) -> Option<Self> {
        match self {
            Self::Keys => Some(Self::Key),
            Self::HlsKeys => Some(Self::HlsKey),
            Self::Outputs => Some(Self::Intent),
            Self::Drms => Some(Self::Leaf(Expected::DrmSystem)),
            _ => None,
        }
    }
}

/// Closed, non-secret view suitable for both CLI and shared Web/Desktop results.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    schema_version: u8,
    file: File,
    path: Vec<Segment>,
    expected: Expected,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    column: Option<u32>,
}
impl Diagnostic {
    /// Check the complete closed schema before trusting a child-process result.
    pub fn valid_for_code(&self, code: &str) -> bool {
        if self.schema_version != 1
            || self.path.len() > 16
            || code
                != match self.file {
                    File::Keys => "key-file-invalid",
                    File::Outputs => "output-spec-invalid",
                }
        {
            return false;
        }
        let mut node = Node::root(self.file);
        for segment in &self.path {
            let next = match *segment {
                Segment::Field(field) => node.field(field),
                Segment::Index(index) if index <= 65_536 => node.index(),
                Segment::Index(_) => None,
            };
            let Some(next) = next else {
                return false;
            };
            node = next;
        }
        node.expected() == self.expected
            && match (self.line, self.column) {
                (None, None) => true,
                (Some(line), Some(column)) => (1..=65_537).contains(&line) && column <= 65_536,
                _ => false,
            }
    }
    /// Build text exclusively from schema tokens, bounded numbers and fixed guidance.
    pub fn message(&self) -> String {
        let mut location = String::new();
        for segment in &self.path {
            match segment {
                Segment::Field(field) => {
                    if !location.is_empty() {
                        location.push('.');
                    }
                    location.push_str(field.name());
                }
                Segment::Index(index) => {
                    use std::fmt::Write as _;
                    let _ = write!(location, "[{index}]");
                }
            }
        }
        if location.is_empty() {
            location.push_str("root");
        }
        let file = match self.file {
            File::Keys => "keys",
            File::Outputs => "outputs",
        };
        let position = self
            .line
            .zip(self.column)
            .map(|(line, column)| format!(" (line {line}, column {column})"))
            .unwrap_or_default();
        format!(
            "{file} configuration at {location}{position}: {} See docs/configuration.md.",
            self.expected.hint()
        )
    }
}

/// An existing error code with optional safe detail; no parser error or user text is retained.
#[derive(Clone, Debug)]
pub struct Failure {
    pub cause: ConversionError,
    pub diagnostic: Diagnostic,
}
impl Failure {
    pub(crate) fn at(file: File, path: impl IntoIterator<Item = Segment>) -> Self {
        let path: Vec<_> = path.into_iter().collect();
        let mut node = Node::root(file);
        for segment in &path {
            node = match *segment {
                Segment::Field(field) => node.field(field),
                Segment::Index(_) => node.index(),
            }
            .expect("configuration locations use known schema paths");
        }
        Self {
            cause: ConversionError(match file {
                File::Keys => "key-file-invalid",
                File::Outputs => "output-spec-invalid",
            }),
            diagnostic: Diagnostic {
                schema_version: 1,
                file,
                path,
                expected: node.expected(),
                line: None,
                column: None,
            },
        }
    }
    pub(crate) fn root(file: File) -> Self {
        Self::at(file, [])
    }
}

/// Accept bounded editor metadata without retaining it or interpreting it as a resource.
pub(crate) fn editor_schema<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<(), D::Error> {
    struct EditorSchema;
    impl serde::de::Visitor<'_> for EditorSchema {
        type Value = ();

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a bounded editor schema reference")
        }

        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<(), E> {
            if value.is_empty()
                || value.len() > 4096
                || !value.bytes().all(|byte| byte.is_ascii_graphic())
            {
                return Err(E::custom("invalid editor schema reference"));
            }
            Ok(())
        }
    }
    deserializer.deserialize_str(EditorSchema)
}

/// Deserialize from the already bounded read, retaining only a safe schema location.
pub(crate) fn parse<T: DeserializeOwned>(bytes: &[u8], file: File) -> Result<T, Failure> {
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let value = serde_path_to_error::deserialize(&mut decoder).map_err(|error| {
        let mut path = Vec::new();
        let mut node = Node::root(file);
        for segment in error.path() {
            if path.len() >= 16 {
                break;
            }
            let next = match segment {
                serde_path_to_error::Segment::Map { key } => {
                    let field = if matches!(node, Node::OutputDomains) {
                        Some(Field::Entry)
                    } else {
                        known_field(key)
                    };
                    field.and_then(|field| {
                        node.field(field).map(|next| (Segment::Field(field), next))
                    })
                }
                serde_path_to_error::Segment::Seq { index } if *index <= 65_536 => {
                    node.index().map(|next| (Segment::Index(*index), next))
                }
                _ => None,
            };
            let Some((safe, next)) = next else {
                break;
            };
            path.push(safe);
            node = next;
        }
        located(Failure::at(file, path), error.inner())
    })?;
    decoder
        .end()
        .map_err(|error| located(Failure::root(file), &error))?;
    Ok(value)
}
fn known_field(name: &str) -> Option<Field> {
    // Match only public schema names. Never store the unknown map key.
    [
        Field::SchemaVersion,
        Field::Input,
        Field::Keys,
        Field::HlsKeys,
        Field::Kid,
        Field::Key,
        Field::Uri,
        Field::Iv,
        Field::Output,
        Field::Outputs,
        Field::OutputDir,
        Field::HlsKeyUri,
        Field::Signaling,
        Field::System,
        Field::LicenseUrl,
        Field::WidevinePssh,
        Field::PlayreadyLicenseUrl,
        Field::Fairplay,
        Field::Id,
        Field::Format,
        Field::Target,
        Field::OutputFormat,
        Field::Encryption,
        Field::Drms,
        Field::Conformance,
        Field::ExternalManifestCheck,
        Field::ManifestProfile,
        Field::Processing,
        Field::DashAddressing,
    ]
    .into_iter()
    .find(|field| field.name() == name)
}
fn located(mut failure: Failure, error: &serde_json::Error) -> Failure {
    if (1..=65_537).contains(&error.line()) && error.column() <= 65_536 {
        failure.diagnostic.line = u32::try_from(error.line()).ok();
        failure.diagnostic.column = u32::try_from(error.column()).ok();
    }
    failure
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversion::{key_file, plan::bundle};
    use serde_json::{Value, json};
    use std::io::Write as _;
    const CANARY: &str = "PRIVATE_CONFIGURATION_CANARY_7e96419a";

    fn assert_safe(error: &Failure) -> Value {
        assert!(error.diagnostic.valid_for_code(error.cause.0));
        for text in [
            format!("{error:?}"),
            error.diagnostic.message(),
            serde_json::to_string(&error.diagnostic).unwrap(),
        ] {
            assert!(!text.contains(CANARY));
            assert!(!text.contains("invalid type:"));
            assert!(!text.contains("unknown field"));
        }
        serde_json::to_value(&error.diagnostic).unwrap()
    }
    fn keys(value: &[u8], path: &Value) {
        let root = crate::native::private_tempdir().unwrap();
        let file = root.path().join(format!("{CANARY}.json"));
        crate::native::create_private_new(&file)
            .unwrap()
            .write_all(value)
            .unwrap();
        let failure = key_file::read_source_diagnostic(&file).unwrap_err();
        assert_eq!(key_file::read_source(&file).unwrap_err(), failure.cause);
        assert_eq!(&assert_safe(&failure)["path"], path);
    }
    #[test]
    fn key_shapes_semantics_and_unknown_names_have_closed_locations() {
        let id = "000102030405060708090a0b0c0d0e0f";
        for (value, path) in [
            (
                json!({"schema_version":1,"input":{"keys":[{"kid":id,"key":false}]}}),
                json!(["input", "keys", 0, "key"]),
            ),
            (
                json!({"schema_version":1,"input":{"keys":[{"kid":CANARY,"key":id}]}}),
                json!(["input", "keys", 0, "kid"]),
            ),
            (
                json!({"schema_version":1,"input":{"hls_keys":[{"uri":CANARY,"key":id,"iv":CANARY}]}}),
                json!(["input", "hls_keys", 0, "iv"]),
            ),
            (
                json!({"schema_version":1,"output":{"kid":id,"key":CANARY}}),
                json!(["output", "key"]),
            ),
            (
                json!({"schema_version":2,"outputs":{CANARY:{"kid":id,"key":false}}}),
                json!(["outputs", "*", "key"]),
            ),
            (
                json!({"schema_version":2,"outputs":{CANARY:{"kid":id,"key":CANARY}}}),
                json!(["outputs", "*", "key"]),
            ),
            (json!({"schema_version":1,CANARY:CANARY}), json!([])),
            (
                json!({"schema_version":1,"input":{CANARY:CANARY}}),
                json!(["input"]),
            ),
            (
                json!({"schema_version":1,"input":{"keys":[{"kid":id,"key":id,CANARY:CANARY}]}}),
                json!(["input", "keys", 0]),
            ),
            (
                json!({"schema_version":1,"output":{"kid":id,"key":id,"signaling":{"system":CANARY}}}),
                json!(["output", "signaling", "system"]),
            ),
            (
                json!({"schema_version":1,"output":{"kid":id,"key":id,"signaling":{"system":"multi-drm","widevine_pssh":CANARY}}}),
                json!(["output", "signaling", "widevine_pssh"]),
            ),
            (
                json!({"schema_version":1,"output":{"kid":id,"key":id,"signaling":{"system":"playready","license_url":CANARY}}}),
                json!(["output", "signaling", "license_url"]),
            ),
            (json!({"schema_version":9}), json!(["schema_version"])),
        ] {
            keys(value.to_string().as_bytes(), &path);
        }
    }
    #[test]
    fn malformed_trailing_duplicate_and_oversized_inputs_keep_refusal_codes() {
        for bytes in [
            b"{".as_slice(),
            b"\xff",
            br#"{"schema_version":1} {}"#,
            br#"{"schema_version":1,"schema_version":1}"#,
        ] {
            keys(bytes, &json!([]));
        }
        keys(&vec![b' '; 65_537], &json!([]));
        let value = json!({"schema_version":1,"input":{"hls_keys":[{"uri":"skd://example","key":"000102030405060708090a0b0c0d0e0f","iv":"0x1"}]}});
        let root = crate::native::private_tempdir().unwrap();
        let path = root.path().join("keys.json");
        crate::native::create_private_new(&path)
            .unwrap()
            .write_all(value.to_string().as_bytes())
            .unwrap();
        assert!(key_file::read_source_diagnostic(&path).is_ok());
    }
    #[test]
    fn recipe_semantics_find_only_public_fields_and_array_positions() {
        for (entry, field) in [
            (
                json!({"id":CANARY,"format":"dash","target":"clear","drms":[]}),
                "id",
            ),
            (
                json!({"id":"dash","format":"dash","target":"clear","drms":["widevine","widevine"]}),
                "drms",
            ),
            (
                json!({"id":"mp4","format":"mp4","target":"clear","drms":[],"conformance":"required"}),
                "conformance",
            ),
            (
                json!({"id":"hls","format":"hls","target":"clear","drms":[],"dash_addressing":"list"}),
                "dash_addressing",
            ),
        ] {
            let root = crate::native::private_tempdir().unwrap();
            let path = root.path().join("outputs.json");
            std::fs::write(
                &path,
                json!({"schema_version":3,"outputs":[entry]}).to_string(),
            )
            .unwrap();
            let error = bundle::read_output_spec_diagnostic(&path).unwrap_err();
            assert_eq!(bundle::read_output_spec(&path).unwrap_err(), error.cause);
            assert_eq!(assert_safe(&error)["path"], json!(["outputs", 0, field]));
        }
    }
    #[test]
    fn projected_diagnostics_validate_schema_context_and_numeric_bounds() {
        let original = json!({"schema_version":1,"file":"keys","path":["input","keys",0,"key"],"expected":"hex-16-bytes","line":1,"column":0});
        let valid: Diagnostic = serde_json::from_value(original.clone()).unwrap();
        assert!(valid.valid_for_code("key-file-invalid"));
        assert!(!valid.valid_for_code("output-spec-invalid"));
        for (field, value) in [
            ("schema_version", json!(2)),
            ("path", json!(["input", "keys", "key"])),
            ("path", json!(["input", "keys", 65_537, "key"])),
            ("expected", json!("output-format")),
            ("line", json!(0)),
            ("column", json!(65_537)),
            ("column", Value::Null),
        ] {
            let mut bad = original.clone();
            bad[field] = value;
            assert!(
                !serde_json::from_value::<Diagnostic>(bad)
                    .unwrap()
                    .valid_for_code("key-file-invalid")
            );
        }
        for (field, value) in [
            ("path", json!([CANARY])),
            ("expected", json!(CANARY)),
            ("file", json!(CANARY)),
            (CANARY, json!(CANARY)),
        ] {
            let mut bad = original.clone();
            bad[field] = value;
            assert!(serde_json::from_value::<Diagnostic>(bad).is_err());
        }
    }
}
