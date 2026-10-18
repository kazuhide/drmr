//! Strict operator configuration; secret strings and input bytes are wiped on drop.

use super::{
    Aes128Key, ContentKey, ConversionError, HlsInputKeys, HlsOutputKey, InputKeys, OutputKey,
    SegmentIv,
};
use serde::Deserialize;
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
#[serde(deny_unknown_fields)]
struct RawSignaling {
    system: String,
    license_url: String,
}
impl Drop for RawSignaling {
    fn drop(&mut self) {
        self.license_url.zeroize();
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
    let file = private_file(path)?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| INVALID)?;
    if bytes.len() as u64 > LIMIT {
        return Err(INVALID);
    }
    let raw: RawFile = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
    if raw.schema_version != 1 {
        return Err(INVALID);
    }
    let input = raw
        .input
        .keys
        .iter()
        .map(|key| ContentKey::from_hex(&key.kid, &key.key))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| INVALID)?;
    let input = InputKeys::new(input).map_err(|_| INVALID)?;
    let hls_input = raw
        .input
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
    let (output, hls_output) = match &raw.output {
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
        .output
        .as_ref()
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
    let signaling = read_signaling(&raw, output.is_some(), sample_hls_uri.is_some())?;
    Ok(OperatorKeys {
        input,
        output,
        hls_input,
        hls_output,
        signaling,
        sample_hls_uri,
    })
}

fn read_signaling(
    raw: &RawFile,
    has_output: bool,
    has_uri: bool,
) -> Result<Option<super::OutputSignaling>, ConversionError> {
    raw.output
        .as_ref()
        .and_then(|key| key.signaling.as_ref())
        .map(|value| {
            if value.system != "playready" || !has_output || has_uri {
                return Err(INVALID);
            }
            super::OutputSignaling::playready(&value.license_url).map_err(|_| INVALID)
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
