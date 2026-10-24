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
    let replacement = if request.format == OutputFormat::Hls {
        Some(Zeroizing::new(Vec::new()))
    } else if let Some(signaling) = &converter.output_signaling {
        Some(
            signaling
                .segment_records(
                    request
                        .output_key
                        .ok_or(ConversionError("output-key-required"))?,
                )?
                .initialization,
        )
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
    let nodes = mp4::parse_boxes(&bytes).map_err(|_| ConversionError("invalid-output-media"))?;
    let mut rewritten = Zeroizing::new(Vec::new());
    for node in &nodes {
        if node.kind() == *b"pssh" {
            continue;
        }
        if node.kind() == *b"moov" {
            let mut payload = Zeroizing::new(Vec::new());
            for child in &node.children {
                if child.kind() != *b"pssh" {
                    payload.extend_from_slice(child.header.bytes(&bytes));
                }
            }
            payload.extend_from_slice(&replacement);
            rewritten.extend(
                u32::try_from(payload.len() + 8)
                    .map_err(|_| ConversionError("resource-budget"))?
                    .to_be_bytes(),
            );
            rewritten.extend(b"moov");
            rewritten.extend_from_slice(&payload);
        } else {
            rewritten.extend_from_slice(node.header.bytes(&bytes));
        }
    }
    if rewritten.len() > 16 * 1024 * 1024 {
        return Err(ConversionError("resource-budget"));
    }
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
    drop(input);
    let mut output = crate::native::create_private_update(path)
        .map_err(|_| ConversionError("output-staging-failed"))?;
    if FileIdentity::read(&output).map_err(|_| ConversionError("output-staging-failed"))?
        != identity
    {
        return Err(ConversionError("native-source-changed"));
    }
    output
        .set_len(0)
        .map_err(|_| ConversionError("output-staging-failed"))?;
    output
        .write_all(&rewritten)
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
    if final_bytes != rewritten {
        return Err(ConversionError("native-source-changed"));
    }
    Ok(())
}
