//! Explicit output settings, independent of key files and immutable saved plans.
use super::{BundleOutputIntent, ManifestValidation};
use crate::{
    conversion::{
        ProcessingMode,
        configuration::{self, Failure, Field as F, File, Segment},
        dash_addressing::DashAddressing,
        files,
        manifest_lint::ManifestProfile,
    },
    inspection::{OutputFormat, TargetProtection},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io::Read as _,
    path::{Component, Path, PathBuf},
};

const LIMIT: usize = 65_536;

/// Current operator settings. Relative destinations become absolute only when read from a file.
#[derive(Clone, Debug)]
pub struct OutputConfig {
    pub output_dir: Option<PathBuf>,
    pub outputs: Vec<BundleOutputIntent>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum PublicFormat {
    Mp4,
    Dash,
    HlsFmp4,
    HlsTs,
}
impl From<PublicFormat> for OutputFormat {
    fn from(value: PublicFormat) -> Self {
        match value {
            PublicFormat::Mp4 => Self::Mp4,
            PublicFormat::Dash => Self::Dash,
            PublicFormat::HlsFmp4 => Self::Hls,
            PublicFormat::HlsTs => Self::HlsTs,
        }
    }
}
impl From<OutputFormat> for PublicFormat {
    fn from(value: OutputFormat) -> Self {
        match value {
            OutputFormat::Mp4 => Self::Mp4,
            OutputFormat::Dash => Self::Dash,
            OutputFormat::Hls => Self::HlsFmp4,
            OutputFormat::HlsTs => Self::HlsTs,
        }
    }
}

fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    schema_version: u32,
    #[serde(default, deserialize_with = "present")]
    output_dir: Option<PathBuf>,
    outputs: Vec<RawOutput>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawOutput {
    id: String,
    output_format: PublicFormat,
    encryption: TargetProtection,
    drms: Vec<crate::conversion::DrmSystem>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    processing: Option<ProcessingMode>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    dash_addressing: Option<DashAddressing>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    external_manifest_check: Option<bool>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    manifest_profile: Option<ManifestProfile>,
}

impl Serialize for OutputConfig {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Document<'a> {
            schema_version: u32,
            #[serde(skip_serializing_if = "Option::is_none")]
            output_dir: Option<&'a Path>,
            outputs: Vec<RawOutput>,
        }
        let normalized = super::normalized(&self.outputs)
            .map_err(|_| serde::ser::Error::custom("invalid output configuration"))?;
        Document {
            schema_version: 4,
            output_dir: self.output_dir.as_deref(),
            outputs: normalized
                .into_iter()
                .map(|output| RawOutput {
                    id: output.id,
                    output_format: output.format.into(),
                    encryption: output.target,
                    drms: output.drms,
                    processing: output.processing,
                    dash_addressing: output.dash_addressing,
                    external_manifest_check: Some(
                        output.conformance == ManifestValidation::Required,
                    ),
                    manifest_profile: output.manifest_profile,
                })
                .collect(),
        }
        .serialize(serializer)
    }
}

/// Parse a bounded canonical document. No file paths are opened or inferred from its values.
pub fn parse_output_config_diagnostic(bytes: &[u8]) -> Result<OutputConfig, Failure> {
    let at = |path: Vec<Segment>| Failure::at(File::Outputs, path);
    if bytes.len() > LIMIT {
        return Err(at(vec![]));
    }
    let raw: RawConfig = configuration::parse(bytes, File::Outputs)?;
    if raw.schema_version != 4 {
        return Err(at(vec![F::SchemaVersion.into()]));
    }
    if raw.output_dir.as_ref().is_some_and(|path| {
        path.as_os_str().is_empty()
            || path.file_name().is_none()
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir))
    }) {
        return Err(at(vec![F::OutputDir.into()]));
    }
    let outputs: Vec<_> = raw
        .outputs
        .into_iter()
        .map(|output| BundleOutputIntent {
            id: output.id,
            format: output.output_format.into(),
            target: output.encryption,
            drms: output.drms,
            conformance: if output.external_manifest_check.unwrap_or(false) {
                ManifestValidation::Required
            } else {
                ManifestValidation::None
            },
            processing: output.processing,
            dash_addressing: output.dash_addressing,
            manifest_profile: output.manifest_profile,
        })
        .collect();
    let outputs = super::normalized(&outputs).map_err(|_| {
        if outputs.is_empty() || outputs.len() > super::MAX_OUTPUTS {
            return at(vec![F::Outputs.into()]);
        }
        let mut ids = BTreeSet::new();
        for (index, output) in outputs.iter().enumerate() {
            let field = if !super::valid_id(&output.id) || !ids.insert(&output.id) {
                Some(F::Id)
            } else if output.conformance == ManifestValidation::Required
                && output.format == OutputFormat::Mp4
            {
                Some(F::ExternalManifestCheck)
            } else if output.manifest_profile.is_some_and(|profile| {
                output.format == OutputFormat::Mp4
                    || profile == ManifestProfile::AppleVod
                        && !matches!(output.format, OutputFormat::Hls | OutputFormat::HlsTs)
            }) {
                Some(F::ManifestProfile)
            } else if output.format != OutputFormat::Dash && output.dash_addressing.is_some() {
                Some(F::DashAddressing)
            } else if output.drms.len() > 3 || {
                let mut systems = output.drms.clone();
                systems.sort();
                systems.windows(2).any(|pair| pair[0] == pair[1])
            } {
                Some(F::Drms)
            } else {
                None
            };
            if field.is_some() || super::normalized(std::slice::from_ref(output)).is_err() {
                let mut path = vec![F::Outputs.into(), Segment::Index(index)];
                path.extend(field.map(Segment::from));
                return at(path);
            }
        }
        at(vec![F::Outputs.into()])
    })?;
    Ok(OutputConfig {
        output_dir: raw.output_dir,
        outputs,
    })
}

/// Read settings once. Relative output directories refer to this file's real parent directory.
/// This resolves placement only; conversion and verification retain their own path guards.
pub fn read_output_config_diagnostic(path: &Path) -> Result<OutputConfig, Failure> {
    let root = || Failure::root(File::Outputs);
    let file = files::regular_input(path).map_err(|_| root())?;
    let mut bytes = Vec::new();
    file.take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| root())?;
    let mut config = parse_output_config_diagnostic(&bytes)?;
    if let Some(output) = &config.output_dir {
        let output = if output.is_absolute() {
            output.clone()
        } else {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            parent.canonicalize().map_err(|_| root())?.join(output)
        };
        config.output_dir = Some(
            output
                .components()
                .filter(|part| !matches!(part, Component::CurDir))
                .collect(),
        );
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write as _;

    fn hls() -> serde_json::Value {
        json!({"schema_version":4,"outputs":[{"id":"hls","output_format":"hls-fmp4","encryption":"clear","drms":[]}]})
    }
    fn parse(value: serde_json::Value) -> Result<OutputConfig, Failure> {
        parse_output_config_diagnostic(&serde_json::to_vec(&value).unwrap())
    }

    #[test]
    fn canonical_names_identify_fragmented_hls_without_changing_internal_format() {
        let parsed = parse(hls()).unwrap();
        assert_eq!(parsed.outputs.len(), 1);
        assert_eq!(parsed.outputs[0].format, OutputFormat::Hls);
        assert!(parsed.output_dir.is_none());
        let serialized = serde_json::to_value(&parsed).unwrap();
        assert_eq!(serialized["schema_version"], 4);
        assert_eq!(serialized["outputs"][0]["output_format"], "hls-fmp4");
        assert_eq!(serialized["outputs"][0]["encryption"], "clear");
        assert_eq!(serialized["outputs"][0]["external_manifest_check"], false);
        assert!(serialized["outputs"][0].get("format").is_none());
    }

    #[test]
    fn old_names_versions_duplicates_null_and_invalid_profiles_are_refused() {
        for patch in [
            json!({"output_format":"hls"}),
            json!({"format":"hls"}),
            json!({"target":"clear"}),
            json!({"conformance":"none"}),
            json!({"manifest_profile":null}),
            json!({"processing":null}),
            json!({"external_manifest_check":null}),
            json!({"external_manifest_check":"false"}),
            json!({"output_format":"dash","manifest_profile":"apple-vod"}),
            json!({"output_format":"mp4","manifest_profile":"base"}),
            json!({"output_format":"mp4","external_manifest_check":true}),
        ] {
            let mut value = hls();
            value["outputs"][0]
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            let error = parse(value).unwrap_err();
            assert!(error.diagnostic.valid_for_code("output-spec-invalid"));
        }
        let mut value = hls();
        value["schema_version"] = json!(3);
        assert!(parse(value).is_err());
        assert!(
            parse_output_config_diagnostic(
                br#"{"schema_version":4,"schema_version":4,"outputs":[]}"#
            )
            .is_err()
        );
        let mut value = hls();
        let duplicate = value["outputs"][0].clone();
        value["outputs"].as_array_mut().unwrap().push(duplicate);
        assert!(parse(value).is_err());
        for output_dir in [json!(null), json!(""), json!("../result")] {
            let mut value = hls();
            value["output_dir"] = output_dir;
            assert!(parse(value).is_err());
        }
    }

    #[test]
    fn relative_destination_uses_config_directory_and_does_not_require_a_new_path() {
        let root = crate::native::private_tempdir().unwrap();
        let config = root.path().join("outputs.json");
        let mut value = hls();
        value["output_dir"] = json!("./existing-result");
        std::fs::create_dir(root.path().join("existing-result")).unwrap();
        crate::native::create_private_new(&config)
            .unwrap()
            .write_all(&serde_json::to_vec(&value).unwrap())
            .unwrap();
        let loaded = read_output_config_diagnostic(&config).unwrap();
        assert_eq!(
            loaded.output_dir,
            Some(root.path().canonicalize().unwrap().join("existing-result"))
        );
    }

    #[test]
    fn diagnostics_never_repeat_unknown_names_or_values() {
        let mut value = hls();
        value["outputs"][0]["PRIVATE_VALUE_CANARY"] = json!("PRIVATE_VALUE_CANARY");
        let error = parse(value).unwrap_err();
        assert!(!format!("{error:?}").contains("PRIVATE_VALUE_CANARY"));
        assert!(!error.diagnostic.message().contains("PRIVATE_VALUE_CANARY"));
        assert!(parse_output_config_diagnostic(&vec![b' '; LIMIT + 1]).is_err());
    }
}
