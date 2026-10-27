//! Portable original segment basenames, independent of filesystems and media processing.
use super::{COLLISION, ConversionError, INVALID, paths};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::{
    collections::BTreeSet,
    fmt,
    path::{Path, PathBuf},
};

const MAX_NAMES: usize = 100_000;
const MAX_NAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_RETAINED_BYTES: usize = 128 * 1024 * 1024;
const LIMIT: ConversionError = ConversionError("source-graph-limit");

/// A compact naming hint. Initialization resources are excluded by the caller.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SegmentNames {
    Sequence {
        prefix: String,
        suffix: String,
        start: u64,
        width: usize,
        count: usize,
    },
    Listed {
        names: Vec<String>,
    },
    Generated,
}

impl fmt::Debug for SegmentNames {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SegmentNames")
            .field("count", &self.count())
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum WireNames {
    Sequence {
        #[serde(deserialize_with = "bounded_text")]
        prefix: String,
        #[serde(deserialize_with = "bounded_text")]
        suffix: String,
        start: u64,
        width: usize,
        count: usize,
    },
    Listed {
        #[serde(deserialize_with = "bounded_names")]
        names: Vec<String>,
    },
    Generated {},
}
impl<'de> Deserialize<'de> for SegmentNames {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = match WireNames::deserialize(deserializer)? {
            WireNames::Sequence {
                prefix,
                suffix,
                start,
                width,
                count,
            } => Self::Sequence {
                prefix,
                suffix,
                start,
                width,
                count,
            },
            WireNames::Listed { names } => Self::Listed { names },
            WireNames::Generated {} => Self::Generated,
        };
        value.validate().map_err(de::Error::custom)?;
        Ok(value)
    }
}

impl SegmentNames {
    /// Infer from ordered source-root-relative media paths, without guessing init names.
    pub fn infer(media: &[PathBuf]) -> Result<Self, ConversionError> {
        if media.len() > MAX_NAMES {
            return Err(LIMIT);
        }
        if media.is_empty() {
            return Ok(Self::Generated);
        }
        let mut names = Vec::with_capacity(media.len());
        let mut bytes = 0usize;
        for path in media {
            paths::relative_components(path)?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or(INVALID)?;
            bytes = bytes
                .checked_add(name.len())
                .filter(|n| *n <= MAX_NAME_BYTES)
                .ok_or(LIMIT)?;
            names.push(name.to_owned());
        }
        validate_list(&names)?;
        Ok(infer_sequence(&names).unwrap_or(Self::Listed { names }))
    }

    /// Validate a saved hint once before emitting an entire segment set.
    pub fn validate(&self) -> Result<(), ConversionError> {
        self.stored_bytes()?;
        match self {
            Self::Sequence {
                prefix,
                suffix,
                start,
                width,
                count,
            } => validate_sequence(prefix, suffix, *start, *width, *count),
            Self::Listed { names } => validate_list(names),
            Self::Generated => Ok(()),
        }
    }

    /// Original segment count, when a source supplied individual segment names.
    pub fn count(&self) -> Option<usize> {
        match self {
            Self::Sequence { count, .. } => Some(*count),
            Self::Listed { names } => Some(names.len()),
            Self::Generated => None,
        }
    }

    /// Native retained allocation, for the enclosing layout's aggregate admission.
    pub fn stored_bytes(&self) -> Result<usize, ConversionError> {
        let extra = match self {
            Self::Sequence { prefix, suffix, .. } => prefix
                .capacity()
                .checked_add(suffix.capacity())
                .ok_or(LIMIT)?,
            Self::Listed { names } => names.iter().try_fold(
                names
                    .capacity()
                    .checked_mul(size_of::<String>())
                    .ok_or(LIMIT)?,
                |sum, name| sum.checked_add(name.capacity()).ok_or(LIMIT),
            )?,
            Self::Generated => 0,
        };
        size_of::<Self>()
            .checked_add(extra)
            .filter(|n| *n <= MAX_RETAINED_BYTES)
            .ok_or(LIMIT)
    }

    /// A decoded filesystem basename. Whole-set validation is performed at admission.
    ///
    /// This remains constant-time per segment after validating a listed hint.
    pub fn file_name(
        &self,
        index: usize,
        output_count: usize,
        extension: &str,
    ) -> Result<String, ConversionError> {
        output_bounds(index, output_count, extension)?;
        let name = match self {
            Self::Sequence {
                prefix,
                suffix,
                start,
                width,
                count,
            } => {
                validate_sequence(prefix, suffix, *start, *width, *count)?;
                validate_sequence(prefix, suffix, *start, *width, output_count)?;
                let number = start
                    .checked_add(u64::try_from(index).map_err(|_| INVALID)?)
                    .ok_or(INVALID)?;
                sequence_name(prefix, suffix, number, *width, extension)
            }
            Self::Listed { names } => {
                valid_count(names.len())?;
                if names.len() == output_count {
                    changed_extension(&names[index], extension)?
                } else {
                    format!("seg_{}.{}", index + 1, extension)
                }
            }
            Self::Generated => format!("seg_{}.{}", index + 1, extension),
        };
        valid_name(&name)?;
        Ok(name)
    }

    /// A URI-ready DASH template and its startNumber, or no representable template.
    ///
    /// Literal dollar signs are doubled; other literal URI bytes are encoded.
    pub fn template(
        &self,
        extension: &str,
        output_count: usize,
    ) -> Result<Option<(String, u64)>, ConversionError> {
        output_bounds(0, output_count, extension)?;
        self.validate()?;
        let sequence = match self {
            Self::Listed { names } if names.len() == output_count => infer_sequence(names),
            Self::Listed { .. } | Self::Generated => Some(Self::Generated),
            Self::Sequence { .. } => Some(self.clone()),
        };
        match sequence {
            Some(Self::Sequence {
                prefix,
                suffix,
                start,
                width,
                ..
            }) => {
                validate_sequence(&prefix, &suffix, start, width, output_count)?;
                // Wider padding is portable as a filename but exceeds the DASH
                // resolver's existing template format limit.
                if width > 12 {
                    return Ok(None);
                }
                let number = if width == 0 {
                    "$Number$".into()
                } else {
                    format!("$Number%0{width}d$")
                };
                Ok(Some((
                    format!(
                        "{}{number}{}.{}",
                        literal_uri(&prefix),
                        literal_uri(&suffix),
                        extension
                    ),
                    start,
                )))
            }
            Some(Self::Generated) => Ok(Some((format!("seg_$Number$.{extension}"), 1))),
            Some(Self::Listed { .. }) | None => Ok(None),
        }
    }
}

fn valid_count(count: usize) -> Result<(), ConversionError> {
    if count == 0 || count > MAX_NAMES {
        Err(LIMIT)
    } else {
        Ok(())
    }
}
fn output_bounds(index: usize, count: usize, extension: &str) -> Result<(), ConversionError> {
    valid_count(count)?;
    if index >= count || !matches!(extension, "ts" | "m4s") {
        Err(INVALID)
    } else {
        Ok(())
    }
}
fn valid_name(name: &str) -> Result<(), ConversionError> {
    if name.contains(['/', '\\']) || paths::relative_components(Path::new(name))?.len() != 1 {
        return Err(INVALID);
    }
    Ok(())
}
fn changed_extension(name: &str, extension: &str) -> Result<String, ConversionError> {
    valid_name(name)?;
    let stem = Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or(INVALID)?;
    let result = format!("{stem}.{extension}");
    valid_name(&result)?;
    Ok(result)
}
fn validate_list(names: &[String]) -> Result<(), ConversionError> {
    valid_count(names.len())?;
    names.iter().try_fold(0usize, |sum, name| {
        sum.checked_add(name.len())
            .filter(|n| *n <= MAX_NAME_BYTES)
            .ok_or(LIMIT)
    })?;
    let mut aliases = BTreeSet::new();
    for name in names {
        let converted = changed_extension(name, "m4s")?;
        if !aliases.insert(paths::alias(Path::new(&converted))?) {
            return Err(COLLISION);
        }
    }
    Ok(())
}
fn sequence_name(prefix: &str, suffix: &str, number: u64, width: usize, ext: &str) -> String {
    format!("{prefix}{number:0width$}{suffix}.{ext}")
}
fn validate_sequence(
    prefix: &str,
    suffix: &str,
    start: u64,
    width: usize,
    count: usize,
) -> Result<(), ConversionError> {
    valid_count(count)?;
    if width > 255
        || width != 0 && width <= start.to_string().len()
        || prefix
            .len()
            .checked_add(suffix.len())
            .is_none_or(|n| n > 255)
    {
        return Err(INVALID);
    }
    let end = start
        .checked_add(u64::try_from(count - 1).map_err(|_| INVALID)?)
        .ok_or(INVALID)?;
    // Only COM1..9/LPT1..9 can change portable-name validity as numbers advance.
    // Endpoints bound length; these small candidates cover device-name crossings.
    for number in [start, end]
        .into_iter()
        .chain((0u64..=9).filter(|n| *n >= start && *n <= end))
    {
        valid_name(&sequence_name(prefix, suffix, number, width, "m4s"))?;
    }
    Ok(())
}
fn number_parts(name: &str) -> Option<(&str, &str, &str, u64)> {
    let stem = Path::new(name).file_stem()?.to_str()?;
    let end = stem.as_bytes().iter().rposition(u8::is_ascii_digit)? + 1;
    let begin = stem.as_bytes()[..end]
        .iter()
        .rposition(|b| !b.is_ascii_digit())
        .map_or(0, |i| i + 1);
    let digits = &stem[begin..end];
    Some((&stem[..begin], &stem[end..], digits, digits.parse().ok()?))
}
fn infer_sequence(names: &[String]) -> Option<SegmentNames> {
    let (prefix, suffix, first, start) = number_parts(names.first()?)?;
    let width = if first.len() > 1 && first.starts_with('0') {
        first.len()
    } else {
        0
    };
    for (index, name) in names.iter().enumerate() {
        let (p, s, digits, number) = number_parts(name)?;
        if p != prefix
            || s != suffix
            || number != start.checked_add(u64::try_from(index).ok()?)?
            || digits != format!("{number:0width$}")
        {
            return None;
        }
    }
    Some(SegmentNames::Sequence {
        prefix: prefix.into(),
        suffix: suffix.into(),
        start,
        width,
        count: names.len(),
    })
}
fn literal_uri(text: &str) -> String {
    crate::source::pyurl::quote_from_bytes(text.as_bytes()).replace("%24", "$$")
}
fn bounded_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value.len() > 255 {
        return Err(de::Error::custom("source name is too long"));
    }
    Ok(value)
}
fn bounded_names<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    struct Names;
    impl<'de> de::Visitor<'de> for Names {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a bounded list of segment basenames")
        }
        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            if seq.size_hint().is_some_and(|n| n > MAX_NAMES) {
                return Err(de::Error::custom("too many segment names"));
            }
            let mut names = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(MAX_NAMES));
            let mut bytes = 0usize;
            while let Some(name) = seq.next_element::<String>()? {
                if names.len() >= MAX_NAMES || name.len() > 255 {
                    return Err(de::Error::custom("segment names exceed their bounds"));
                }
                bytes = bytes
                    .checked_add(name.len())
                    .filter(|n| *n <= MAX_NAME_BYTES)
                    .ok_or_else(|| de::Error::custom("segment names exceed their byte bound"))?;
                names.push(name);
            }
            Ok(names)
        }
    }
    deserializer.deserialize_seq(Names)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn infer(names: &[&str]) -> SegmentNames {
        SegmentNames::infer(&names.iter().map(PathBuf::from).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn original_bbb_sequence_preserves_start_and_literal_padding() {
        let names = infer(&["video/r000600.ts", "video/r000601.ts", "video/r000602.ts"]);
        assert_eq!(
            names,
            SegmentNames::Sequence {
                prefix: "r".into(),
                suffix: String::new(),
                start: 600,
                width: 6,
                count: 3
            }
        );
        assert_eq!(names.file_name(0, 3, "m4s").unwrap(), "r000600.m4s");
        assert_eq!(names.file_name(3, 4, "ts").unwrap(), "r000603.ts");
        assert_eq!(
            names.template("m4s", 3).unwrap(),
            Some(("r$Number%06d$.m4s".into(), 600))
        );
    }

    #[test]
    fn unpadded_numbers_cross_decimal_widths_and_padded_numbers_extend() {
        let names = infer(&["seg_9.ts", "seg_10.ts"]);
        assert_eq!(names.file_name(2, 3, "m4s").unwrap(), "seg_11.m4s");
        assert_eq!(
            names.template("ts", 3).unwrap(),
            Some(("seg_$Number$.ts".into(), 9))
        );
        let names = infer(&["seg_09.ts", "seg_10.ts"]);
        assert_eq!(
            names.template("ts", 3).unwrap(),
            Some(("seg_$Number%02d$.ts".into(), 9))
        );
        assert_eq!(names.file_name(0, 1, "ts").unwrap(), "seg_09.ts");
    }

    #[test]
    fn rightmost_number_keeps_prefix_and_stem_suffix() {
        let names = infer(&["v2_seg0007_audio.ts", "v2_seg0008_audio.ts"]);
        assert_eq!(
            names.file_name(1, 2, "m4s").unwrap(),
            "v2_seg0008_audio.m4s"
        );
        assert_eq!(
            names.template("m4s", 2).unwrap(),
            Some(("v2_seg$Number%04d$_audio.m4s".into(), 7))
        );
    }

    #[test]
    fn arbitrary_and_noncontiguous_names_keep_order_or_generate_after_stitch() {
        for source in [
            vec!["intro.ts", "ad-break.ts"],
            vec!["part_3.ts", "part_5.ts"],
        ] {
            let names = infer(&source);
            assert!(matches!(names, SegmentNames::Listed { .. }));
            assert_eq!(
                names.file_name(1, 2, "m4s").unwrap(),
                source[1].replace(".ts", ".m4s")
            );
            assert_eq!(names.template("m4s", 2).unwrap(), None);
            assert_eq!(names.file_name(2, 3, "ts").unwrap(), "seg_3.ts");
            assert_eq!(
                names.template("ts", 3).unwrap(),
                Some(("seg_$Number$.ts".into(), 1))
            );
        }
    }

    #[test]
    fn empty_hints_generate_but_media_names_do_not_guess_initialization_roles() {
        assert_eq!(infer(&[]), SegmentNames::Generated);
        let mp4_segment = infer(&["video/input.mp4"]);
        assert!(matches!(mp4_segment, SegmentNames::Listed { .. }));
        assert_eq!(mp4_segment.file_name(0, 1, "m4s").unwrap(), "input.m4s");
        let numbered_mp4_segment = infer(&["video/r000600.mp4"]);
        assert_eq!(
            numbered_mp4_segment.file_name(0, 1, "m4s").unwrap(),
            "r000600.m4s"
        );
        let names = infer(&["init_01.ts", "init_02.ts"]);
        assert_eq!(names.count(), Some(2));
        assert_eq!(names.file_name(0, 2, "m4s").unwrap(), "init_01.m4s");
    }

    #[test]
    fn singleton_numeric_hint_and_wide_literal_padding_are_bounded() {
        let names = infer(&["part_9.ts"]);
        assert_eq!(names.file_name(1, 2, "m4s").unwrap(), "part_10.m4s");
        let wide = format!("r{}1.ts", "0".repeat(199));
        let names = infer(&[&wide]);
        assert_eq!(
            names.file_name(0, 1, "m4s").unwrap(),
            wide.replace(".ts", ".m4s")
        );
        assert!(names.stored_bytes().unwrap() < 1024);
        assert_eq!(names.template("m4s", 1).unwrap(), None);
    }

    #[test]
    fn dollar_percent_unicode_and_spaces_are_literal_template_bytes() {
        let names = infer(&["音 声$%_001_tail$.ts", "音 声$%_002_tail$.ts"]);
        assert_eq!(
            names.file_name(0, 2, "m4s").unwrap(),
            "音 声$%_001_tail$.m4s"
        );
        assert_eq!(
            names.template("m4s", 2).unwrap(),
            Some((
                "%E9%9F%B3%20%E5%A3%B0$$%25_$Number%03d$_tail$$.m4s".into(),
                1
            ))
        );
    }

    #[test]
    fn unsafe_names_and_extension_normalization_aliases_are_refused() {
        for name in [
            "../a.ts",
            "/a.ts",
            "a\\b.ts",
            "nul.ts",
            "COM1.ts",
            "a:stream.ts",
            "a\0.ts",
        ] {
            assert!(
                SegmentNames::infer(&[PathBuf::from(name)]).is_err(),
                "{name:?}"
            );
        }
        for pair in [
            ["A.ts", "a.ts"],
            ["same.ts", "same.m4s"],
            ["café.ts", "cafe\u{301}.ts"],
        ] {
            assert_eq!(
                SegmentNames::infer(&pair.map(PathBuf::from)).unwrap_err(),
                COLLISION
            );
        }
    }

    #[test]
    fn arithmetic_bounds_and_output_extensions_fail_before_formatting() {
        let names = SegmentNames::Sequence {
            prefix: "r".into(),
            suffix: String::new(),
            start: u64::MAX,
            width: 0,
            count: 1,
        };
        names.validate().unwrap();
        assert!(names.file_name(0, 2, "m4s").is_err());
        assert!(names.file_name(usize::MAX, 1, "ts").is_err());
        assert!(names.file_name(0, MAX_NAMES + 1, "ts").is_err());
        for ext in ["mp4", ".ts", "TS", "../ts"] {
            assert!(names.file_name(0, 1, ext).is_err());
        }
        let unsafe_range = SegmentNames::Sequence {
            prefix: "COM".into(),
            suffix: String::new(),
            start: 0,
            width: 0,
            count: 1,
        };
        unsafe_range.validate().unwrap();
        assert!(unsafe_range.template("ts", 11).is_err());
    }

    #[test]
    fn strict_serde_rejects_unknown_null_and_noncanonical_fields() {
        for value in [
            serde_json::json!(null),
            serde_json::json!({"kind":"generated","extra":1}),
            serde_json::json!({"kind":"listed","names":null}),
            serde_json::json!({"kind":"listed","names":[]}),
            serde_json::json!({"kind":"listed","names":["../bad.ts"]}),
            serde_json::json!({"kind":"sequence","prefix":null,"suffix":"","start":1,"width":0,"count":1}),
            serde_json::json!({"kind":"sequence","prefix":"r","suffix":"","start":10,"width":2,"count":1}),
            serde_json::json!({"kind":"sequence","prefix":"r","suffix":"","start":1,"width":usize::MAX,"count":1}),
        ] {
            assert!(
                serde_json::from_value::<SegmentNames>(value.clone()).is_err(),
                "accepted noncanonical value: {value}"
            );
        }
        for names in [
            infer(&["r0001.ts"]),
            infer(&["one.ts", "two.ts"]),
            SegmentNames::Generated,
        ] {
            assert_eq!(
                serde_json::from_value::<SegmentNames>(serde_json::to_value(&names).unwrap())
                    .unwrap(),
                names
            );
        }
    }

    #[test]
    fn stored_listed_sequences_can_supply_a_template() {
        let names = SegmentNames::Listed {
            names: vec!["r001.ts".into(), "r002.ts".into()],
        };
        assert_eq!(
            names.template("m4s", 2).unwrap(),
            Some(("r$Number%03d$.m4s".into(), 1))
        );
    }

    #[test]
    fn count_and_aggregate_name_bytes_are_bounded() {
        assert_eq!(
            SegmentNames::infer(&vec![PathBuf::from("a.ts"); MAX_NAMES + 1]).unwrap_err(),
            LIMIT
        );
        let suffix = "x".repeat(245);
        let names = (0..=(MAX_NAME_BYTES / 254))
            .map(|i| format!("{i:06}{suffix}.ts"))
            .collect();
        assert_eq!(
            SegmentNames::Listed { names }.validate().unwrap_err(),
            LIMIT
        );
        let value = serde_json::json!({"kind":"listed","names":vec!["a.ts";MAX_NAMES+1]});
        assert!(serde_json::from_value::<SegmentNames>(value).is_err());
    }
}
