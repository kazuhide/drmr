//! Normalize fresh private producer artifacts with the existing streamed cipher writer.
use super::{BAD, ConversionError, InputKeys, SegmentTransformer, init, read};
use crate::{
    concurrency::Stop,
    conversion::{ContentKey, OutputKey, files, private_file_mode},
    native::{self, FileIdentity},
};
use std::{
    io::{Read as _, Seek as _, SeekFrom, Write as _},
    path::{Path, PathBuf},
};

fn keys(key: &OutputKey) -> Result<InputKeys, ConversionError> {
    InputKeys::new(vec![ContentKey {
        id: key.0.id,
        bytes: key.0.bytes.clone(),
    }])
}

fn identity(path: &Path) -> Result<FileIdentity, ConversionError> {
    FileIdentity::read(&files::regular_input(path)?).map_err(|_| BAD)
}

fn replace(
    source: &Path,
    target: &Path,
    expected: &FileIdentity,
    stop: &Stop,
) -> Result<(), ConversionError> {
    read::check_stop(stop)?;
    if identity(target)? != *expected {
        return Err(ConversionError("native-source-changed"));
    }
    std::fs::rename(source, target).map_err(|_| BAD)
}

pub(in crate::conversion) fn reprotect_generated_file(
    path: &Path,
    key: &OutputKey,
    stop: &Stop,
) -> Result<(), ConversionError> {
    private_file_mode(path)?;
    let expected = identity(path)?;
    let work = files::Stage::new(path.parent())?;
    let initialization = work.path().join("init.mp4");
    let media = work.path().join("media.m4s");
    let transformer =
        SegmentTransformer::reprotect_generated(path, &initialization, key, true, stop)?;
    transformer.rewrite_segment(path, &media, &keys(key)?, stop)?;
    let combined = work.path().join("complete.mp4");
    let mut output = files::create_new(&combined)?;
    let mut buffer = vec![0_u8; super::CHUNK_BYTES];
    for part in [&initialization, &media] {
        let mut input = files::regular_input(part)?;
        loop {
            read::check_stop(stop)?;
            let count = input.read(&mut buffer).map_err(|_| BAD)?;
            if count == 0 {
                break;
            }
            output.write_all(&buffer[..count]).map_err(|_| BAD)?;
        }
    }
    output.sync_all().map_err(|_| BAD)?;
    drop(output);
    replace(&combined, path, &expected, stop)
}

pub(in crate::conversion) fn reprotect_generated_rendition(
    initialization: &Path,
    resources: &[PathBuf],
    key: &OutputKey,
    stop: &Stop,
) -> Result<(), ConversionError> {
    private_file_mode(initialization)?;
    let expected_init = identity(initialization)?;
    let work = files::Stage::new(initialization.parent())?;
    let new_init = work.path().join("init.mp4");
    let transformer =
        SegmentTransformer::reprotect_generated(initialization, &new_init, key, false, stop)?;
    let mut session = transformer.session()?;
    let keys = keys(key)?;
    let mut rewritten = Vec::with_capacity(resources.len());
    for (index, source) in resources.iter().enumerate() {
        read::check_stop(stop)?;
        let output = work.path().join(format!("resource-{index}.m4s"));
        let expected = identity(source)?;
        session.rewrite_segment(source, &output, &keys, stop)?;
        rewritten.push((output, expected));
    }
    drop(session);
    for ((source, expected), target) in rewritten.iter().zip(resources) {
        replace(source, target, expected, stop)?;
    }
    replace(&new_init, initialization, &expected_init, stop)
}

/// Retain a declared level only when every other typed `vpcC` field is unchanged.
pub(in crate::conversion) fn restore_vp9_level(
    path: &Path,
    expected: &[u8],
    stop: &Stop,
) -> Result<(), ConversionError> {
    crate::conversion::vp9::Configuration::from_vpcc(expected)?;
    private_file_mode(path)?;
    let mut reader = read::Reader::open(path, stop, false)?;
    let layout = reader.layout()?;
    let parsed = init::parse(&layout)?;
    let [track] = parsed.tracks.as_slice() else {
        return Err(BAD);
    };
    let actual = &track.descriptor.decoder_config;
    crate::conversion::vp9::Configuration::from_vpcc(actual)?;
    if track.descriptor.codec != *b"vp09"
        || actual
            .iter()
            .zip(expected)
            .enumerate()
            .any(|(index, (a, b))| index != 5 && a != b)
    {
        return Err(ConversionError("native-vp9-configuration-mismatch"));
    }
    if actual[5] == expected[5] {
        return reader.finish();
    }
    let entry = read::one(&track.table.children, *b"stsd")?
        .children
        .first()
        .ok_or(BAD)?;
    let config = read::one(&entry.children, *b"vpcC")?;
    let at = layout
        .movie_offset
        .checked_add(
            u64::try_from(config.header.offset + config.header.header_size + 5).map_err(|_| BAD)?,
        )
        .ok_or(BAD)?;
    let identity = FileIdentity::read(&reader.file).map_err(|_| BAD)?;
    reader.finish()?;
    let mut output = native::open_media_update(path).map_err(|_| BAD)?;
    if FileIdentity::read(&output).map_err(|_| BAD)? != identity {
        return Err(BAD);
    }
    output
        .seek(SeekFrom::Start(layout.movie_offset))
        .map_err(|_| BAD)?;
    let mut held = vec![0; layout.movie.len()];
    output.read_exact(&mut held).map_err(|_| BAD)?;
    if held != layout.movie {
        return Err(BAD);
    }
    read::check_stop(stop)?;
    output.seek(SeekFrom::Start(at)).map_err(|_| BAD)?;
    output.write_all(&expected[5..6]).map_err(|_| BAD)?;
    output.sync_all().map_err(|_| BAD)?;
    read::check_stop(stop)
}
