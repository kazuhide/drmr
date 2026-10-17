//! Confined local fragmented-MP4 playlists, with operator-supplied segment keys only.
use super::{
    Aes128Key, ConversionError, ConversionReport, Converter, HlsInputKeys, HlsOutputKey, InputKeys,
    OutputKey, SegmentIv,
};
use crate::{
    inspection::{self, Inspection, Protection, TargetProtection},
    m3u8::parse_attributes,
};
use std::{
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
};
const MAX_RESOURCES: usize = 4096;

pub(super) struct Localized {
    pub combined: PathBuf,
    pub reference: PathBuf,
    pub report: Inspection,
}
struct Playlist {
    combined: File,
    reference: String,
    sequence: u64,
    count: u64,
    resources: usize,
    bytes: u64,
    has_map: bool,
    ended: bool,
    pending_duration: bool,
    singleton_tags: std::collections::BTreeSet<String>,
}
#[derive(Default)]
struct Cipher<'a> {
    key: Option<&'a Aes128Key>,
    iv: Option<SegmentIv>,
}
fn attributes(value: &str) -> Result<crate::m3u8::Attributes, ConversionError> {
    parse_attributes(value).map_err(|_| ConversionError("hls-invalid-attributes"))
}
fn private_new(path: &Path) -> Result<File, ConversionError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|_| ConversionError("temporary-file-unavailable"))
}
fn local_resource(root: &Path, uri: &str) -> Result<PathBuf, ConversionError> {
    if uri.is_empty()
        || uri.contains([':', '\\', '?', '#', '\0'])
        || Path::new(uri)
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(ConversionError("hls-resource-not-confined-local"));
    }
    let path = root.join(uri);
    if fs::symlink_metadata(&path)
        .map_err(|_| ConversionError("hls-resource-unreadable"))?
        .file_type()
        .is_symlink()
    {
        return Err(ConversionError("hls-resource-not-confined-local"));
    }
    let path = path
        .canonicalize()
        .map_err(|_| ConversionError("hls-resource-unreadable"))?;
    if !path.starts_with(root) {
        return Err(ConversionError("hls-resource-not-confined-local"));
    }
    Ok(path)
}
impl Playlist {
    fn resource(
        &mut self,
        root: &Path,
        uri: &str,
        stage: &Path,
        cipher: &Cipher<'_>,
        map: bool,
    ) -> Result<String, ConversionError> {
        self.resources += 1;
        if self.resources > MAX_RESOURCES {
            return Err(ConversionError("hls-resource-count-limit"));
        }
        let original = local_resource(root, uri)?;
        let metadata =
            fs::metadata(&original).map_err(|_| ConversionError("hls-resource-unreadable"))?;
        self.bytes = self
            .bytes
            .checked_add(metadata.len())
            .ok_or(ConversionError("media-size-limit"))?;
        if self.bytes > super::MAX_MEDIA_BYTES {
            return Err(ConversionError("media-size-limit"));
        }
        let name = format!(
            "resource-{:04}.{}",
            self.resources,
            if map { "mp4" } else { "m4s" }
        );
        let copied = stage.join(&name);
        super::copy_private(&original, &copied)?;
        let plaintext = if let Some(key) = cipher.key {
            let derived;
            let iv = if let Some(iv) = &cipher.iv {
                iv
            } else {
                if map {
                    return Err(ConversionError("hls-encrypted-map-explicit-iv-required"));
                }
                derived = SegmentIv::for_sequence(
                    self.sequence
                        .checked_add(self.count)
                        .ok_or(ConversionError("hls-media-sequence-overflow"))?,
                );
                &derived
            };
            let key_name = format!("key-{:04}.bin", self.resources);
            private_new(&stage.join(&key_name))?
                .write_all(key.0.as_ref())
                .map_err(|_| ConversionError("temporary-key-write-failed"))?;
            writeln!(
                self.reference,
                "#EXT-X-KEY:METHOD=AES-128,URI=\"{key_name}\",IV=0x{}",
                hex::encode(iv.0.as_ref())
            )
            .expect("writing a string cannot fail");
            let clear = stage.join(format!("clear-{:04}", self.resources));
            super::decrypt_segment(&copied, &clear, key, iv)?;
            clear
        } else {
            self.reference.push_str("#EXT-X-KEY:METHOD=NONE\n");
            copied
        };
        let mut input =
            File::open(plaintext).map_err(|_| ConversionError("hls-resource-unreadable"))?;
        std::io::copy(&mut input, &mut self.combined)
            .map_err(|_| ConversionError("media-copy-failed"))?;
        Ok(name)
    }
}
struct Localizer<'a> {
    root: PathBuf,
    stage: &'a Path,
    keys: &'a HlsInputKeys,
    playlist: Playlist,
    cipher: Cipher<'a>,
    saw_sample: bool,
    saw_aes: bool,
}
impl Localizer<'_> {
    fn set_key(&mut self, value: &str) -> Result<(), ConversionError> {
        let attrs = attributes(value)?;
        match attrs.get("METHOD") {
            Some("NONE") if attrs.len() == 1 => self.cipher = Cipher::default(),
            Some("AES-128") => {
                if attrs
                    .get("KEYFORMAT")
                    .is_some_and(|format| format != "identity")
                {
                    return Err(ConversionError("hls-key-format-not-supported"));
                }
                let uri = attrs
                    .get("URI")
                    .ok_or(ConversionError("hls-key-uri-missing"))?;
                let key = self
                    .keys
                    .0
                    .iter()
                    .find(|(candidate, _)| candidate.as_str() == uri)
                    .map(|(_, key)| key)
                    .ok_or(ConversionError("hls-input-key-required"))?;
                self.cipher = Cipher {
                    key: Some(key),
                    iv: attrs.get("IV").map(SegmentIv::from_hex).transpose()?,
                };
                self.saw_aes = true;
            }
            Some("SAMPLE-AES" | "SAMPLE-AES-CTR") => {
                if attrs.get("URI").is_none_or(str::is_empty) {
                    return Err(ConversionError("hls-key-uri-missing"));
                }
                self.cipher = Cipher::default();
                self.saw_sample = true;
            }
            _ => return Err(ConversionError("hls-key-method-not-supported")),
        }
        Ok(())
    }
    fn set_map(&mut self, value: &str) -> Result<(), ConversionError> {
        if self.playlist.has_map || self.playlist.count > 0 {
            return Err(ConversionError("hls-map-changes-not-supported"));
        }
        let attrs = attributes(value)?;
        if attrs.get("BYTERANGE").is_some() {
            return Err(ConversionError("hls-byte-ranges-not-yet-supported"));
        }
        let uri = attrs
            .get("URI")
            .ok_or(ConversionError("hls-map-uri-missing"))?;
        let name = self
            .playlist
            .resource(&self.root, uri, self.stage, &self.cipher, true)?;
        writeln!(self.playlist.reference, "#EXT-X-MAP:URI=\"{name}\"")
            .expect("writing a string cannot fail");
        self.playlist.has_map = true;
        Ok(())
    }
    fn duration(&mut self, value: &str) -> Result<(), ConversionError> {
        let duration = value
            .split_once(',')
            .ok_or(ConversionError("hls-invalid-duration"))?
            .0;
        if self.playlist.pending_duration
            || duration.len() > 64
            || !duration
                .parse::<f64>()
                .is_ok_and(|duration| duration.is_finite() && duration > 0.0)
        {
            return Err(ConversionError("hls-invalid-duration"));
        }
        writeln!(self.playlist.reference, "#EXTINF:{duration},")
            .expect("writing a string cannot fail");
        self.playlist.pending_duration = true;
        Ok(())
    }
    fn line(&mut self, line: &str) -> Result<(), ConversionError> {
        if self.playlist.ended {
            return Err(ConversionError("hls-data-after-endlist"));
        }
        if line.contains('\0') {
            return Err(ConversionError("hls-invalid-line"));
        }
        let (name, value) = line.split_once(':').unwrap_or((line, ""));
        if matches!(
            name,
            "#EXT-X-VERSION"
                | "#EXT-X-TARGETDURATION"
                | "#EXT-X-MEDIA-SEQUENCE"
                | "#EXT-X-PLAYLIST-TYPE"
        ) && !self.playlist.singleton_tags.insert(name.into())
        {
            return Err(ConversionError("hls-duplicate-header"));
        }
        match name {
            "#EXT-X-VERSION" => {
                if !value
                    .parse::<u64>()
                    .is_ok_and(|version| (1..=10).contains(&version))
                {
                    return Err(ConversionError("hls-version-not-supported"));
                }
            }
            "#EXT-X-PLAYLIST-TYPE" => {
                if value != "VOD" {
                    return Err(ConversionError("hls-complete-vod-required"));
                }
            }
            "#EXT-X-INDEPENDENT-SEGMENTS"
            | "#EXT-X-PROGRAM-DATE-TIME"
            | "#EXT-X-BITRATE"
            | "#EXT-X-ALLOW-CACHE" => {}
            "#EXT-X-KEY" => self.set_key(value)?,
            "#EXT-X-MAP" => self.set_map(value)?,
            "#EXTINF" => self.duration(value)?,
            "#EXT-X-MEDIA-SEQUENCE" => {
                if self.playlist.count > 0 {
                    return Err(ConversionError("hls-invalid-media-sequence"));
                }
                self.playlist.sequence = value
                    .parse()
                    .map_err(|_| ConversionError("hls-invalid-media-sequence"))?;
                writeln!(
                    self.playlist.reference,
                    "#EXT-X-MEDIA-SEQUENCE:{}",
                    self.playlist.sequence
                )
                .expect("writing a string cannot fail");
            }
            "#EXT-X-TARGETDURATION" => {
                let duration = value
                    .parse::<u64>()
                    .map_err(|_| ConversionError("hls-invalid-target-duration"))?;
                if duration == 0 {
                    return Err(ConversionError("hls-invalid-target-duration"));
                }
                writeln!(self.playlist.reference, "#EXT-X-TARGETDURATION:{duration}")
                    .expect("writing a string cannot fail");
            }
            "#EXT-X-ENDLIST" => {
                self.playlist.ended = true;
                self.playlist.reference.push_str("#EXT-X-ENDLIST\n");
            }
            "#EXT-X-BYTERANGE" => return Err(ConversionError("hls-byte-ranges-not-yet-supported")),
            "#EXT-X-DISCONTINUITY" | "#EXT-X-GAP" | "#EXT-X-PART" => {
                return Err(ConversionError("hls-presentation-feature-not-supported"));
            }
            "#EXT-X-STREAM-INF" | "#EXT-X-MEDIA" => {
                return Err(ConversionError("hls-master-not-yet-supported"));
            }
            _ if !line.starts_with('#') => {
                if !self.playlist.has_map {
                    return Err(ConversionError(
                        "hls-transport-stream-input-not-yet-supported",
                    ));
                }
                if !self.playlist.pending_duration {
                    return Err(ConversionError("hls-segment-duration-missing"));
                }
                let name =
                    self.playlist
                        .resource(&self.root, line, self.stage, &self.cipher, false)?;
                writeln!(self.playlist.reference, "{name}").expect("writing a string cannot fail");
                self.playlist.count += 1;
                self.playlist.pending_duration = false;
            }
            _ if name.starts_with("#EXT-X-") => {
                return Err(ConversionError("hls-tag-not-supported"));
            }
            _ => {}
        }
        Ok(())
    }
    fn finish(self, path: &Path, combined: PathBuf) -> Result<Localized, ConversionError> {
        let playlist = self.playlist;
        if !playlist.ended
            || playlist.pending_duration
            || playlist.count == 0
            || !playlist.singleton_tags.contains("#EXT-X-TARGETDURATION")
        {
            return Err(ConversionError("hls-complete-vod-required"));
        }
        playlist
            .combined
            .sync_all()
            .map_err(|_| ConversionError("media-copy-failed"))?;
        drop(playlist.combined);
        let actual = inspection::inspect(&combined)
            .map_err(|_| ConversionError("invalid-localized-hls-media"))?;
        if self.saw_aes && actual.protection != Protection::Clear {
            return Err(ConversionError("hls-nested-protection-not-supported"));
        }
        if self.saw_sample && !matches!(actual.protection, Protection::Cenc | Protection::Cbcs) {
            return Err(ConversionError("hls-sample-protection-unresolved"));
        }
        let reference = if self.saw_aes {
            let reference = self.stage.join("reference.m3u8");
            private_new(&reference)?
                .write_all(playlist.reference.as_bytes())
                .map_err(|_| ConversionError("temporary-file-unavailable"))?;
            reference
        } else {
            combined.clone()
        };
        let report =
            inspection::inspect(path).map_err(|_| ConversionError("hls-inspection-failed"))?;
        Ok(Localized {
            combined,
            reference,
            report,
        })
    }
}
pub(super) fn localize(
    path: &Path,
    stage: &Path,
    keys: &HlsInputKeys,
) -> Result<Localized, ConversionError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ConversionError("input-unreadable"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(ConversionError("input-not-regular-file"));
    }
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let root = parent
        .canonicalize()
        .map_err(|_| ConversionError("input-unreadable"))?;
    let bytes = inspection::read_manifest(path)
        .map_err(|_| ConversionError("hls-manifest-unreadable-or-too-large"))?;
    let text = std::str::from_utf8(&bytes).map_err(|_| ConversionError("hls-invalid-utf8"))?;
    let mut lines = text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    if lines.next() != Some("#EXTM3U") {
        return Err(ConversionError("hls-invalid-header"));
    }
    let combined = stage.join("combined.mp4");
    let playlist = Playlist {
        combined: private_new(&combined)?,
        reference: "#EXTM3U\n#EXT-X-VERSION:7\n".into(),
        sequence: 0,
        count: 0,
        resources: 0,
        bytes: 0,
        has_map: false,
        ended: false,
        pending_duration: false,
        singleton_tags: std::collections::BTreeSet::new(),
    };
    let mut localizer = Localizer {
        root,
        stage,
        keys,
        playlist,
        cipher: Cipher::default(),
        saw_sample: false,
        saw_aes: false,
    };
    for line in lines {
        localizer.line(line)?;
    }
    localizer.finish(path, combined)
}
impl Converter {
    pub(super) fn hls_output(
        &self,
        input: &Path,
        output: &Path,
        input_keys: &InputKeys,
        output_key: &HlsOutputKey,
    ) -> Result<ConversionReport, ConversionError> {
        let destination = super::destination(output)?;
        let workspace = super::workspace()?;
        let source = workspace.path().join("source.mp4");
        super::copy_private(input, &source)?;
        let input_report = super::validate(&source, input_keys)?;
        self.backend.profile(&source)?;
        let clear = workspace.path().join("clear.mp4");
        self.backend.package(
            &source,
            &clear,
            (input_report.protection != Protection::Clear).then_some(input_keys),
            None,
        )?;
        let clear_hls = workspace.path().join("clear-hls");
        fs::create_dir(&clear_hls).map_err(|_| ConversionError("temporary-file-unavailable"))?;
        self.backend.package_hls(&clear, &clear_hls)?;
        let staged = tempfile::Builder::new()
            .prefix(".drmr-hls-")
            .tempdir_in(
                destination
                    .parent()
                    .ok_or(ConversionError("output-parent-unavailable"))?,
            )
            .map_err(|_| ConversionError("output-staging-failed"))?;
        encrypt_presentation(&clear_hls, staged.path(), output_key)?;
        let verify_stage = workspace.path().join("verify");
        fs::create_dir(&verify_stage).map_err(|_| ConversionError("temporary-file-unavailable"))?;
        let verification_keys = HlsInputKeys::new(vec![(
            output_key.key_uri().into(),
            Aes128Key::from_hex(&hex::encode(output_key.key.0.as_ref()))?,
        )])?;
        let localized = localize(
            &staged.path().join("media.m3u8"),
            &verify_stage,
            &verification_keys,
        )?;
        let verification = self.backend.verify_media(
            &source,
            &localized.reference,
            super::source_key(&input_report, input_keys),
            None,
        )?;
        if !verification.passed {
            return Err(ConversionError("round-trip-verification-failed"));
        }
        let output_report = inspection::inspect(&staged.path().join("media.m3u8"))
            .map_err(|_| ConversionError("hls-inspection-failed"))?;
        publish_directory(staged.path(), &destination)?;
        let _ = staged.keep();
        Ok(ConversionReport {
            schema_version: 1,
            input: input_report,
            output: output_report,
            verification,
        })
    }
    pub(super) fn hls_input(
        &self,
        input: &Path,
        output: &Path,
        target: TargetProtection,
        input_keys: &InputKeys,
        hls_keys: &HlsInputKeys,
        output_key: Option<&OutputKey>,
    ) -> Result<ConversionReport, ConversionError> {
        let destination = super::destination(output)?;
        let workspace = super::workspace()?;
        let localized = localize(input, workspace.path(), hls_keys)?;
        let candidate = workspace.path().join("candidate.mp4");
        let mut report = self.convert_mp4(
            &localized.combined,
            &candidate,
            target,
            input_keys,
            output_key,
        )?;
        let actual = super::validate(&localized.combined, input_keys)?;
        let output_keys = output_key
            .map(|key| {
                InputKeys::new(vec![super::ContentKey::from_hex(
                    &key.0.id_hex(),
                    &key.0.key_hex(),
                )?])
            })
            .transpose()?
            .unwrap_or_default();
        let result = super::validate(&candidate, &output_keys)?;
        report.verification = self.backend.verify_media(
            &localized.reference,
            &candidate,
            super::source_key(&actual, input_keys),
            super::source_key(&result, &output_keys),
        )?;
        if !report.verification.passed {
            return Err(ConversionError("round-trip-verification-failed"));
        }
        report.input = localized.report;
        super::publish(&candidate, &destination)?;
        Ok(report)
    }
}
fn encrypt_presentation(
    clear_hls: &Path,
    stage: &Path,
    output_key: &HlsOutputKey,
) -> Result<(), ConversionError> {
    let playlist = fs::read_to_string(clear_hls.join("media.m3u8"))
        .map_err(|_| ConversionError("packaged-hls-playlist-missing"))?;
    let mut encrypted = String::new();
    let mut count = 0;
    for line in playlist.lines() {
        let (name, value) = line.split_once(':').unwrap_or((line, ""));
        if name == "#EXT-X-MAP" {
            let attrs = attributes(value)?;
            let uri = attrs
                .get("URI")
                .ok_or(ConversionError("packaged-hls-map-missing"))?;
            let path = local_resource(
                &clear_hls
                    .canonicalize()
                    .map_err(|_| ConversionError("temporary-file-unavailable"))?,
                uri,
            )?;
            let iv = SegmentIv::random()?;
            output_key.encrypt(&path, &stage.join("init.mp4"), &iv)?;
            write!(
                encrypted,
                "#EXT-X-KEY:METHOD=AES-128,URI=\"{}\",IV=0x{}\n#EXT-X-MAP:URI=\"init.mp4\"\n",
                output_key.key_uri(),
                hex::encode(iv.0.as_ref())
            )
            .expect("writing a string cannot fail");
        } else if !line.starts_with('#') && !line.is_empty() {
            count += 1;
            if count > MAX_RESOURCES {
                return Err(ConversionError("hls-resource-count-limit"));
            }
            let source = local_resource(
                &clear_hls
                    .canonicalize()
                    .map_err(|_| ConversionError("temporary-file-unavailable"))?,
                line,
            )?;
            let name = format!("segment-{count:06}.m4s");
            let iv = SegmentIv::random()?;
            output_key.encrypt(&source, &stage.join(&name), &iv)?;
            write!(
                encrypted,
                "#EXT-X-KEY:METHOD=AES-128,URI=\"{}\",IV=0x{}\n{name}\n",
                output_key.key_uri(),
                hex::encode(iv.0.as_ref())
            )
            .expect("writing a string cannot fail");
        } else {
            encrypted.push_str(line);
            encrypted.push('\n');
        }
    }
    if count == 0 {
        return Err(ConversionError("packaged-hls-empty"));
    }
    private_new(&stage.join("media.m3u8"))?
        .write_all(encrypted.as_bytes())
        .map_err(|_| ConversionError("output-staging-failed"))?;
    super::copy_private(&clear_hls.join("master.m3u8"), &stage.join("master.m3u8"))?;
    Ok(())
}
fn publish_directory(source: &Path, destination: &Path) -> Result<(), ConversionError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            source,
            rustix::fs::CWD,
            destination,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(|_| ConversionError("output-exists-or-publish-failed"))?;
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (source, destination);
        Err(ConversionError("private-files-not-supported-on-platform"))
    }
}
