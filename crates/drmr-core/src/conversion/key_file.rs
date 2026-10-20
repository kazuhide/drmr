//! Strict operator configuration; secret strings and input bytes are wiped on drop.

use super::{
    Aes128Key, ContentKey, ConversionError, HlsInputKeys, HlsOutputKey, InputKeys, OutputKey,
    SegmentIv,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use zeroize::{Zeroize, Zeroizing};

const LIMIT: u64 = 65_536;
const INVALID: ConversionError = ConversionError("key-file-invalid");

/// Separate decryption and encryption domains; no environment fallback is applied.
#[derive(Default)]
pub struct OperatorKeys {
    pub input: InputKeys,
    pub output: Option<OutputKey>,
    pub hls_input: HlsInputKeys,
    pub hls_output: Option<HlsOutputKey>,
    pub signaling: Option<super::OutputSignaling>,
    sample_hls_uri: Option<Zeroizing<String>>,
}

/// One shared source-key domain and separately named encryption/signaling domains.
#[derive(Debug, Default)]
pub struct BundleOperatorKeys {
    pub input: InputKeys,
    pub hls_input: HlsInputKeys,
    pub outputs: BTreeMap<String, OperatorKeys>,
}

impl std::fmt::Debug for OperatorKeys {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OperatorKeys")
            .field("input", &self.input)
            .field("output", &self.output)
            .field("hls_input", &self.hls_input)
            .field("hls_output", &self.hls_output)
            .field("signaling", &self.signaling)
            .finish_non_exhaustive()
    }
}

impl OperatorKeys {
    /// An operator-selected playlist location, never included in diagnostics or reports.
    pub fn hls_output_uri(&self) -> Option<&str> {
        self.sample_hls_uri
            .as_deref()
            .map(String::as_str)
            .or_else(|| self.hls_output.as_ref().map(HlsOutputKey::key_uri))
    }
    /// Match output HLS playlist declarations without retrieving the advertised URI.
    pub fn output_hls_verification_keys(&self) -> Result<HlsInputKeys, ConversionError> {
        self.hls_output.as_ref().map_or_else(
            || Ok(HlsInputKeys::default()),
            |key| {
                HlsInputKeys::new_with_ivs(vec![(
                    key.uri.to_string(),
                    Aes128Key(Zeroizing::new(*key.key.0)),
                    key.iv.as_ref().map(|iv| SegmentIv(Zeroizing::new(*iv.0))),
                )])
            },
        )
    }
    /// Decrypt a result for comparison using its output domain, independently of source keys.
    pub fn output_verification_keys(&self) -> Result<InputKeys, ConversionError> {
        self.output.as_ref().map_or_else(
            || Ok(InputKeys::default()),
            |key| {
                InputKeys::new(vec![ContentKey::from_hex(
                    &key.0.id_hex(),
                    &key.0.key_hex(),
                )?])
            },
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    schema_version: u32,
    #[serde(default)]
    input: RawInput,
    output: Option<RawOutput>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBundle {
    schema_version: u32,
    #[serde(default)]
    input: RawInput,
    #[serde(deserialize_with = "unique_outputs")]
    outputs: BTreeMap<String, RawOutput>,
}

fn unique_outputs<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, RawOutput>, D::Error> {
    struct Outputs;
    impl<'de> serde::de::Visitor<'de> for Outputs {
        type Value = BTreeMap<String, RawOutput>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("unique bounded output identifiers")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut values = BTreeMap::new();
            while let Some((name, output)) = map.next_entry::<String, RawOutput>()? {
                if values.len() >= 16
                    || name.is_empty()
                    || name.len() > 64
                    || !name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                    || !name.as_bytes()[0].is_ascii_alphanumeric()
                    || values.insert(name, output).is_some()
                {
                    return Err(serde::de::Error::custom("invalid output identifiers"));
                }
            }
            Ok(values)
        }
    }
    deserializer.deserialize_map(Outputs)
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInput {
    #[serde(default)]
    keys: Vec<RawKey>,
    #[serde(default)]
    hls_keys: Vec<RawHlsKey>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHlsKey {
    uri: String,
    key: String,
    iv: Option<String>,
}

impl Drop for RawHlsKey {
    fn drop(&mut self) {
        self.uri.zeroize();
        self.key.zeroize();
        if let Some(iv) = &mut self.iv {
            iv.zeroize();
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOutput {
    kid: Option<String>,
    key: String,
    hls_key_uri: Option<String>,
    iv: Option<String>,
    signaling: Option<RawSignaling>,
}

#[derive(Deserialize)]
#[serde(tag = "system", deny_unknown_fields)]
enum RawSignaling {
    #[serde(rename = "playready")]
    PlayReady { license_url: String },
    #[serde(rename = "multi-drm")]
    MultiDrm {
        widevine_pssh: Option<String>,
        playready_license_url: Option<String>,
        #[serde(default)]
        fairplay: bool,
    },
}
impl Drop for RawSignaling {
    fn drop(&mut self) {
        match self {
            Self::PlayReady { license_url } => license_url.zeroize(),
            Self::MultiDrm {
                widevine_pssh,
                playready_license_url,
                ..
            } => {
                if let Some(value) = widevine_pssh {
                    value.zeroize();
                }
                if let Some(value) = playready_license_url {
                    value.zeroize();
                }
            }
        }
    }
}

impl Drop for RawOutput {
    fn drop(&mut self) {
        self.key.zeroize();
        if let Some(iv) = &mut self.iv {
            iv.zeroize();
        }
        if let Some(uri) = &mut self.hls_key_uri {
            uri.zeroize();
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKey {
    kid: String,
    key: String,
}

impl Drop for RawKey {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

/// Read at most 64 KiB from a protected, regular, singly linked operator file.
pub fn read(path: &Path) -> Result<OperatorKeys, ConversionError> {
    let bytes = read_bytes(path)?;
    parse_single(&bytes)
}

fn parse_single(bytes: &[u8]) -> Result<OperatorKeys, ConversionError> {
    let raw: RawFile = serde_json::from_slice(bytes).map_err(|_| INVALID)?;
    if raw.schema_version != 1 {
        return Err(INVALID);
    }
    let (input, hls_input) = parse_input(&raw.input)?;
    let mut output = parse_output(raw.output.as_ref())?;
    output.input = input;
    output.hls_input = hls_input;
    Ok(output)
}

/// Read a strict schema-version-2 file. Output identifiers carry no path authority.
pub fn read_bundle(path: &Path) -> Result<BundleOperatorKeys, ConversionError> {
    let bytes = read_bytes(path)?;
    parse_bundle(&bytes)
}

fn parse_bundle(bytes: &[u8]) -> Result<BundleOperatorKeys, ConversionError> {
    let raw: RawBundle = serde_json::from_slice(bytes).map_err(|_| INVALID)?;
    if raw.schema_version != 2 {
        return Err(INVALID);
    }
    let (input, hls_input) = parse_input(&raw.input)?;
    let outputs = raw
        .outputs
        .iter()
        .map(|(id, output)| Ok((id.clone(), parse_output(Some(output))?)))
        .collect::<Result<_, ConversionError>>()?;
    Ok(BundleOperatorKeys {
        input,
        hls_input,
        outputs,
    })
}

/// Read source decryption keys from either strict operator schema.
/// The complete file is validated; output domains are discarded rather than inferred.
/// Conversion and verification must still use the schema matching their output contract.
pub fn read_source(path: &Path) -> Result<OperatorKeys, ConversionError> {
    #[derive(Deserialize)]
    struct Version {
        schema_version: u32,
    }
    let bytes = read_bytes(path)?;
    let version: Version = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
    let (input, hls_input) = match version.schema_version {
        1 => {
            let keys = parse_single(&bytes)?;
            (keys.input, keys.hls_input)
        }
        2 => {
            let keys = parse_bundle(&bytes)?;
            (keys.input, keys.hls_input)
        }
        _ => return Err(INVALID),
    };
    Ok(OperatorKeys {
        input,
        hls_input,
        ..OperatorKeys::default()
    })
}

fn read_bytes(path: &Path) -> Result<Zeroizing<Vec<u8>>, ConversionError> {
    let file = private_file(path)?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| INVALID)?;
    if bytes.len() as u64 > LIMIT {
        return Err(INVALID);
    }
    Ok(bytes)
}

fn parse_input(raw: &RawInput) -> Result<(InputKeys, HlsInputKeys), ConversionError> {
    let input = raw
        .keys
        .iter()
        .map(|key| ContentKey::from_hex(&key.kid, &key.key))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| INVALID)?;
    let input = InputKeys::new(input).map_err(|_| INVALID)?;
    let hls_input = raw
        .hls_keys
        .iter()
        .map(|key| {
            Ok((
                key.uri.clone(),
                Aes128Key::from_hex(&key.key).map_err(|_| INVALID)?,
                key.iv
                    .as_deref()
                    .map(SegmentIv::from_hex)
                    .transpose()
                    .map_err(|_| INVALID)?,
            ))
        })
        .collect::<Result<Vec<_>, ConversionError>>()?;
    let hls_input = HlsInputKeys::new_with_ivs(hls_input).map_err(|_| INVALID)?;
    Ok((input, hls_input))
}
fn parse_output(raw: Option<&RawOutput>) -> Result<OperatorKeys, ConversionError> {
    let (output, hls_output) = match raw {
        None => (None, None),
        Some(key) => match (&key.kid, &key.hls_key_uri) {
            (Some(_), _) if key.iv.is_some() => return Err(INVALID),
            (Some(kid), _) => (
                Some(OutputKey::from_hex(kid, &key.key).map_err(|_| INVALID)?),
                None,
            ),
            (None, Some(uri)) => (
                None,
                Some(
                    match key.iv.as_deref() {
                        Some(iv) => HlsOutputKey::new_with_iv(
                            uri.clone(),
                            Aes128Key::from_hex(&key.key).map_err(|_| INVALID)?,
                            SegmentIv::from_hex(iv).map_err(|_| INVALID)?,
                        ),
                        None => HlsOutputKey::new(
                            uri.clone(),
                            Aes128Key::from_hex(&key.key).map_err(|_| INVALID)?,
                        ),
                    }
                    .map_err(|_| INVALID)?,
                ),
            ),
            _ => return Err(INVALID),
        },
    };
    let sample_hls_uri = raw
        .filter(|key| key.kid.is_some())
        .and_then(|key| key.hls_key_uri.as_ref())
        .map(|uri| {
            if uri.is_empty()
                || uri.len() > 4096
                || uri.contains('"')
                || uri.chars().any(char::is_control)
            {
                Err(INVALID)
            } else {
                Ok(Zeroizing::new(uri.clone()))
            }
        })
        .transpose()?;
    let signaling = read_signaling(
        raw,
        output.is_some(),
        sample_hls_uri.is_some(),
        hls_output.is_some(),
    )?;
    if let Some(value) = &signaling
        && value.is_multi()
    {
        if let Some(key) = &output {
            value.multi_objects(key).map_err(|_| INVALID)?;
        }
        value
            .validate_uri(
                sample_hls_uri
                    .as_deref()
                    .map(String::as_str)
                    .or_else(|| hls_output.as_ref().map(HlsOutputKey::key_uri)),
            )
            .map_err(|_| INVALID)?;
    }
    Ok(OperatorKeys {
        output,
        hls_output,
        signaling,
        sample_hls_uri,
        ..OperatorKeys::default()
    })
}

fn read_signaling(
    raw: Option<&RawOutput>,
    has_output: bool,
    has_uri: bool,
    has_hls: bool,
) -> Result<Option<super::OutputSignaling>, ConversionError> {
    raw.and_then(|key| key.signaling.as_ref())
        .map(|value| match value {
            RawSignaling::PlayReady { license_url } => {
                if !has_output || has_uri {
                    return Err(INVALID);
                }
                super::OutputSignaling::playready(license_url).map_err(|_| INVALID)
            }
            RawSignaling::MultiDrm {
                widevine_pssh,
                playready_license_url,
                fairplay,
            } => {
                if !has_output
                    && (!has_hls
                        || !*fairplay
                        || widevine_pssh.is_some()
                        || playready_license_url.is_some())
                    || *fairplay && !has_uri && !has_hls
                {
                    return Err(INVALID);
                }
                super::OutputSignaling::multi_drm(
                    widevine_pssh.as_deref(),
                    playready_license_url.as_deref(),
                    *fairplay,
                )
                .map_err(|_| INVALID)
            }
        })
        .transpose()
}

fn private_file(path: &Path) -> Result<std::fs::File, ConversionError> {
    let file = crate::native::open_private_read(path).map_err(|_| INVALID)?;
    if file.metadata().map_err(|_| INVALID)?.len() > LIMIT {
        return Err(INVALID);
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_loader_validates_both_schemas_without_selecting_an_output_domain() {
        use std::io::Write as _;
        let root = crate::native::private_tempdir().unwrap();
        for version in [1, 2] {
            let path = root.path().join(format!("keys-{version}.json"));
            let mut value = serde_json::json!({"schema_version":version,"input":{"hls_keys":[{"uri":"skd://fixture","key":"11111111111111111111111111111111"}]}});
            let output = serde_json::json!({"kid":"22222222222222222222222222222222","key":"33333333333333333333333333333333"});
            if version == 1 {
                value["output"] = output;
            } else {
                value["outputs"] = serde_json::json!({"dash":output});
            }
            crate::native::create_private_new(&path)
                .unwrap()
                .write_all(value.to_string().as_bytes())
                .unwrap();
            let source = read_source(&path).unwrap();
            assert_eq!(source.hls_input.0.len(), 1);
            assert!(
                source.output.is_none()
                    && source.hls_output.is_none()
                    && source.signaling.is_none()
            );
            if version == 2 {
                assert_eq!(read(&path).unwrap_err(), INVALID);
            }
            let malformed = root.path().join(format!("invalid-{version}.json"));
            if version == 1 {
                value["output"]["key"] = "invalid".into();
            } else {
                value["outputs"]["dash"]["key"] = "invalid".into();
            }
            crate::native::create_private_new(&malformed)
                .unwrap()
                .write_all(value.to_string().as_bytes())
                .unwrap();
            assert_eq!(read_source(&malformed).unwrap_err(), INVALID);
        }
    }

    #[test]
    fn output_verification_keeps_its_own_associated_iv_for_the_same_uri() {
        let input = HlsInputKeys::new_with_ivs(vec![(
            "skd://same-uri".into(),
            Aes128Key::from_hex("11111111111111111111111111111111").unwrap(),
            Some(SegmentIv::from_hex("22222222222222222222222222222222").unwrap()),
        )])
        .unwrap();
        let keys = OperatorKeys {
            hls_input: input,
            hls_output: Some(
                HlsOutputKey::new_with_iv(
                    "skd://same-uri",
                    Aes128Key::from_hex("33333333333333333333333333333333").unwrap(),
                    SegmentIv::from_hex("44444444444444444444444444444444").unwrap(),
                )
                .unwrap(),
            ),
            ..OperatorKeys::default()
        };
        let result = keys.output_hls_verification_keys().unwrap();
        assert_eq!(keys.hls_input.0.len(), 1);
        assert_eq!(result.0.len(), 1);
        assert_eq!(&*keys.hls_input.0[0].uri, &*result.0[0].uri);
        assert_eq!(*keys.hls_input.0[0].key.0, [0x11; 16]);
        assert_eq!(*keys.hls_input.0[0].iv.as_ref().unwrap().0, [0x22; 16]);
        assert_eq!(*result.0[0].key.0, [0x33; 16]);
        assert_eq!(*result.0[0].iv.as_ref().unwrap().0, [0x44; 16]);
        let legacy = OperatorKeys {
            hls_output: Some(
                HlsOutputKey::new(
                    "key.bin",
                    Aes128Key::from_hex("55555555555555555555555555555555").unwrap(),
                )
                .unwrap(),
            ),
            ..OperatorKeys::default()
        };
        assert!(
            legacy.output_hls_verification_keys().unwrap().0[0]
                .iv
                .is_none()
        );
    }
}
