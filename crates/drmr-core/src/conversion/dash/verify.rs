//! Bind a saved output intent to the actual final addressing, independently of media comparison.
use super::{
    ConversionError, DASH_NS, Document, MAX_SEGMENTS, Path,
    addressing::{self, Kind},
    children, error, index, one, reference,
};
use crate::{
    concurrency::Stop,
    conversion::{dash_addressing::DashAddressing, files},
};

const MISMATCH: ConversionError = ConversionError("output-addressing-verification-failed");

/// Check the addressing choice without copying media or requiring encryption keys.
/// The ordinary independent verifier still proves track IDs, clocks, payloads and signaling.
pub(crate) fn verify_output_addressing(
    root: &Path,
    expected: DashAddressing,
    stop: &Stop,
) -> Result<(), ConversionError> {
    verify(root, expected, stop).map_err(|failure| {
        if failure.0 == "cancelled" {
            failure
        } else {
            MISMATCH
        }
    })
}

fn verify(root: &Path, expected: DashAddressing, stop: &Stop) -> Result<(), ConversionError> {
    super::checkpoint(Some(stop))?;
    let root = root.canonicalize().map_err(|_| MISMATCH)?;
    let bytes = files::read_manifest(&root.join("manifest.mpd"))?;
    super::source_references(&bytes)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| MISMATCH)?;
    let document = Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            nodes_limit: 20_000,
            ..Default::default()
        },
    )
    .map_err(|_| MISMATCH)?;
    let mpd = document.root_element();
    if !mpd.has_tag_name((DASH_NS, "MPD")) {
        return Err(MISMATCH);
    }
    let period = one(mpd, "Period")?.ok_or(MISMATCH)?;
    let representations: Vec<_> = children(period, "AdaptationSet")
        .into_iter()
        .flat_map(|set| children(set, "Representation"))
        .collect();
    if representations.len()
        != document
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "Representation")
            .count()
    {
        return Err(MISMATCH);
    }
    let mut remaining = MAX_SEGMENTS;
    for rep in representations {
        super::checkpoint(Some(stop))?;
        let resolved = addressing::resolve(rep, remaining)?;
        let matches = match expected {
            DashAddressing::Template => {
                resolved.kind == Kind::Template && resolved.nominal.is_none()
            }
            DashAddressing::TemplateDuration => {
                resolved.kind == Kind::Template && resolved.nominal.is_some()
            }
            DashAddressing::List => resolved.kind == Kind::List && resolved.nominal.is_none(),
            DashAddressing::Base => resolved.kind == Kind::Base,
        };
        if !matches {
            return Err(MISMATCH);
        }
        let count = if let Some(resource) = &resolved.index {
            let path = reference(&root, &resolved.base, &resource.uri)?;
            let range = resource.range.ok_or(MISMATCH)?;
            let parsed = index::read(&path, range, &resource.uri, remaining, stop)?;
            if parsed.timescale != resolved.timescale {
                return Err(MISMATCH);
            }
            index::check_layout(
                &path,
                resolved.initialization.range.ok_or(MISMATCH)?,
                range,
                &parsed.segments,
                stop,
            )?;
            parsed.segments.len()
        } else {
            resolved.segments.len()
        };
        remaining = remaining
            .checked_sub(count)
            .ok_or(error("dash-segment-limit"))?;
    }
    super::checkpoint(Some(stop))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_reported_before_opening_a_missing_output() {
        let owner = files::Stage::new(None).unwrap();
        let stop = Stop::new();
        stop.set();
        assert_eq!(
            verify_output_addressing(&owner.path().join("missing"), DashAddressing::Base, &stop),
            Err(ConversionError("cancelled"))
        );
        assert_eq!(
            verify_output_addressing(
                &owner.path().join("missing"),
                DashAddressing::Base,
                &Stop::new()
            ),
            Err(MISMATCH)
        );
    }

    #[test]
    fn inherited_addressing_is_checked_but_foreign_or_unbound_representations_are_rejected() {
        let owner = files::Stage::new(None).unwrap();
        let manifest = owner.path().join("manifest.mpd");
        let template = r#"<SegmentTemplate duration="2" endNumber="3" initialization="init.mp4" media="$Number$.m4s"/>"#;
        let valid = format!(
            r#"<MPD xmlns="{DASH_NS}" type="static" mediaPresentationDuration="PT6S"><Period><AdaptationSet>{template}<Representation id="1" bandwidth="100"/></AdaptationSet></Period></MPD>"#
        );
        fs_write(&manifest, &valid);
        verify_output_addressing(owner.path(), DashAddressing::TemplateDuration, &Stop::new())
            .unwrap();
        for extra in [
            "<Representation xmlns=\"urn:foreign\"/>",
            "<Group><Representation/></Group>",
        ] {
            let text = valid.replace("</AdaptationSet>", &format!("{extra}</AdaptationSet>"));
            fs_write(&manifest, &text);
            assert_eq!(
                verify_output_addressing(
                    owner.path(),
                    DashAddressing::TemplateDuration,
                    &Stop::new()
                ),
                Err(MISMATCH)
            );
        }
    }

    fn fs_write(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
    }
}
