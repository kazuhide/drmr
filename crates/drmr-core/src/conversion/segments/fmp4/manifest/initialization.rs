//! Provider records are replaced only after every resource writer has finished.
use super::{ConversionError, ConversionRequest, Converter, OutputFormat, conversion};
use crate::{mp4, native::FileIdentity};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::Path;
use zeroize::Zeroizing;

pub(super) fn finish(
    converter: &Converter,
    path: &Path,
    request: &ConversionRequest<'_>,
) -> Result<(), ConversionError> {
    converter.backend.check_cancelled()?;
    let replacement = if let Some(signaling) = &converter.output_signaling {
        Some(
            signaling
                .segment_records(
                    request
                        .output_key
                        .ok_or(ConversionError("output-key-required"))?,
                    request.target,
                )?
                .initialization,
        )
    } else if request.format == OutputFormat::Hls {
        Some(Zeroizing::new(Vec::new()))
    } else {
        None
    };
    let Some(replacement) = replacement else {
        return Ok(());
    };
    let mut input = conversion::files::regular_input(path)?;
    let identity =
        FileIdentity::read(&input).map_err(|_| ConversionError("output-staging-failed"))?;
    let length = input
        .metadata()
        .map_err(|_| ConversionError("output-staging-failed"))?
        .len();
    if length > 16 * 1024 * 1024 {
        return Err(ConversionError("resource-budget"));
    }
    let mut bytes = Zeroizing::new(vec![
        0;
        usize::try_from(length)
            .map_err(|_| ConversionError("resource-budget"))?
    ]);
    input
        .read_exact(&mut bytes)
        .map_err(|_| ConversionError("output-staging-failed"))?;
    let rewritten = replace_records(&bytes, &replacement)?;
    converter.backend.check_cancelled()?;
    input
        .seek(SeekFrom::Start(0))
        .map_err(|_| ConversionError("output-staging-failed"))?;
    let mut observed = Zeroizing::new(vec![0; bytes.len()]);
    input
        .read_exact(&mut observed)
        .map_err(|_| ConversionError("output-staging-failed"))?;
    if bytes != observed
        || FileIdentity::read(&conversion::files::regular_input(path)?)
            .map_err(|_| ConversionError("output-staging-failed"))?
            != identity
    {
        return Err(ConversionError("native-source-changed"));
    }
    drop(observed);
    drop(input);
    rewrite_existing(converter, path, identity, &bytes, &rewritten)
}

fn rewrite_existing(
    converter: &Converter,
    path: &Path,
    identity: FileIdentity,
    expected: &[u8],
    rewritten: &[u8],
) -> Result<(), ConversionError> {
    converter.backend.check_cancelled()?;
    let mut output = crate::native::open_media_update(path)
        .map_err(|_| ConversionError("output-staging-failed"))?;
    if FileIdentity::read(&output).map_err(|_| ConversionError("output-staging-failed"))?
        != identity
        || output
            .metadata()
            .map_err(|_| ConversionError("output-staging-failed"))?
            .len()
            != expected.len() as u64
    {
        return Err(ConversionError("native-source-changed"));
    }
    let mut current = Zeroizing::new(vec![0; expected.len()]);
    output
        .read_exact(&mut current)
        .map_err(|_| ConversionError("output-staging-failed"))?;
    if current.as_slice() != expected {
        return Err(ConversionError("native-source-changed"));
    }
    drop(current);
    converter.backend.check_cancelled()?;
    output
        .seek(SeekFrom::Start(0))
        .map_err(|_| ConversionError("output-staging-failed"))?;
    output
        .set_len(0)
        .map_err(|_| ConversionError("output-staging-failed"))?;
    output
        .write_all(rewritten)
        .and_then(|()| output.sync_all())
        .map_err(|_| ConversionError("output-staging-failed"))?;
    drop(output);
    converter.backend.check_cancelled()?;
    let mut final_input = conversion::files::regular_input(path)?;
    if FileIdentity::read(&final_input).map_err(|_| ConversionError("output-staging-failed"))?
        != identity
        || final_input
            .metadata()
            .map_err(|_| ConversionError("output-staging-failed"))?
            .len()
            != rewritten.len() as u64
    {
        return Err(ConversionError("native-source-changed"));
    }
    let mut final_bytes = Zeroizing::new(vec![0; rewritten.len()]);
    final_input
        .read_exact(&mut final_bytes)
        .map_err(|_| ConversionError("output-staging-failed"))?;
    if final_bytes.as_slice() != rewritten {
        return Err(ConversionError("native-source-changed"));
    }
    Ok(())
}

fn replace_records(
    bytes: &[u8],
    replacement: &[u8],
) -> Result<Zeroizing<Vec<u8>>, ConversionError> {
    let nodes = mp4::parse_boxes(bytes).map_err(|_| ConversionError("invalid-output-media"))?;
    let mut rewritten = Zeroizing::new(Vec::new());
    for node in &nodes {
        if node.kind() == *b"pssh" {
            continue;
        }
        if node.kind() == *b"moov" {
            let mut payload = Zeroizing::new(Vec::new());
            for child in &node.children {
                if child.kind() != *b"pssh" {
                    payload.extend_from_slice(child.header.bytes(bytes));
                }
            }
            payload.extend_from_slice(replacement);
            rewritten.extend(
                u32::try_from(payload.len() + 8)
                    .map_err(|_| ConversionError("resource-budget"))?
                    .to_be_bytes(),
            );
            rewritten.extend(b"moov");
            rewritten.extend_from_slice(&payload);
        } else {
            rewritten.extend_from_slice(node.header.bytes(bytes));
        }
    }
    if rewritten.len() > 16 * 1024 * 1024 {
        return Err(ConversionError("resource-budget"));
    }
    Ok(rewritten)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        conversion::{HlsInputKeys, InputKeys, files::Stage},
        events::NoSink,
        toolrun::{Secrets, ToolRunner},
    };
    use std::sync::Arc;

    fn converter() -> Converter {
        Converter::new(
            Arc::new(ToolRunner::new(
                Arc::new(Secrets::default()),
                Arc::new(NoSink),
            )),
            "must-not-run",
            "must-not-run",
        )
    }

    fn media(stage: &Stage, bytes: &[u8]) -> (std::path::PathBuf, FileIdentity) {
        let path = stage.path().join("init.mp4");
        let mut file = conversion::files::create_new(&path).unwrap();
        file.write_all(bytes).unwrap();
        let identity = FileIdentity::read(&file).unwrap();
        drop(file);
        (path, identity)
    }

    #[test]
    fn same_object_changes_between_read_and_update_are_refused_without_truncation() {
        let stage = Stage::new(None).unwrap();
        let (path, identity) = media(&stage, b"original");
        std::fs::write(&path, b"modified").unwrap();
        assert!(
            FileIdentity::read(&conversion::files::regular_input(&path).unwrap()).unwrap()
                == identity
        );
        let result = rewrite_existing(&converter(), &path, identity, b"original", b"replacement");
        assert_eq!(result, Err(ConversionError("native-source-changed")));
        assert_eq!(std::fs::read(&path).unwrap(), b"modified");
    }

    #[test]
    fn an_existing_foreign_replacement_is_refused_before_any_contents_change() {
        let stage = Stage::new(None).unwrap();
        let (path, identity) = media(&stage, b"original");
        std::fs::rename(&path, stage.path().join("retained.mp4")).unwrap();
        conversion::files::create_new(&path)
            .unwrap()
            .write_all(b"foreign!")
            .unwrap();
        assert_eq!(
            rewrite_existing(&converter(), &path, identity, b"original", b"replacement"),
            Err(ConversionError("native-source-changed"))
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"foreign!");
        assert_eq!(
            std::fs::read(stage.path().join("retained.mp4")).unwrap(),
            b"original"
        );
    }

    #[test]
    fn finishing_initialization_in_a_prepared_media_parent_retains_identity_and_exact_bytes() {
        fn boxed(kind: [u8; 4], bytes: &[u8]) -> Vec<u8> {
            [
                (u32::try_from(bytes.len()).unwrap() + 8)
                    .to_be_bytes()
                    .as_slice(),
                kind.as_slice(),
                bytes,
            ]
            .concat()
        }
        let control = boxed(*b"free", b"retained control");
        let pssh = crate::playready::build_pssh_box(
            &crate::playready::WIDEVINE_SYSTEM_ID,
            &[
                0x12, 0x10, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
            ],
        )
        .unwrap();
        let input = [
            boxed(*b"ftyp", b"isom\0\0\0\0isomiso6"),
            boxed(*b"moov", &[control.clone(), pssh].concat()),
        ]
        .concat();
        let expected = [
            boxed(*b"ftyp", b"isom\0\0\0\0isomiso6"),
            boxed(*b"moov", &control),
        ]
        .concat();
        let stage = Stage::new(None).unwrap();
        let (path, identity) = media(&stage, &input);
        let keys = InputKeys::default();
        let hls = HlsInputKeys::default();
        let request = ConversionRequest {
            input: &path,
            output: stage.path(),
            format: OutputFormat::Hls,
            target: crate::inspection::TargetProtection::Clear,
            input_keys: &keys,
            hls_input_keys: &hls,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        };
        finish(&converter(), &path, &request).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        assert!(
            FileIdentity::read(&conversion::files::regular_input(&path).unwrap()).unwrap()
                == identity
        );
    }
}
