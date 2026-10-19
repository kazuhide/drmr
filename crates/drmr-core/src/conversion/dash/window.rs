//! Exact presented sample windows and one canonical decimal serialization of the period end.
use super::{
    ConversionError, Node, Path, attribute, children, decimal_duration, error, expand, integer,
    one, reference, representation_scopes, sequence,
};
use std::io::Write as _;
const NANOS: u64 = 1_000_000_000;
type Rational = (u64, u64);
fn earlier(a: Rational, b: Rational) -> bool {
    u128::from(a.0) * u128::from(b.1) < u128::from(b.0) * u128::from(a.1)
}
fn equal(a: Rational, b: Rational) -> bool {
    u128::from(a.0) * u128::from(b.1) == u128::from(b.0) * u128::from(a.1)
}
#[derive(Clone, Copy)]
pub(super) struct Window {
    origin: Rational,
    first: Rational,
    end: Rational,
}
impl Window {
    pub(super) fn read(
        templates: &[Node<'_, '_>],
        media: (u64, u64, u64),
    ) -> Result<Self, ConversionError> {
        let offset = integer(attribute(templates, "presentationTimeOffset"), 0)?;
        let scale = integer(attribute(templates, "timescale"), 1)?;
        if scale == 0 || media.2 == 0 || media.0 >= media.1 {
            return Err(error("dash-presentation-offset-or-timescale-not-supported"));
        }
        Ok(Self {
            origin: (offset, scale),
            first: (media.0, media.2),
            end: (media.1, media.2),
        })
    }
}
#[derive(Default)]
pub(super) struct Presentation {
    origin: Option<Rational>,
    first: Option<Rational>,
    end: Option<Rational>,
}
impl Presentation {
    pub(super) fn include(&mut self, window: Window) -> Result<(), ConversionError> {
        if self.origin.is_some_and(|old| !equal(old, window.origin)) {
            return Err(error("dash-incoherent-presentation-offset"));
        }
        self.origin = Some(window.origin);
        if self.first.is_none_or(|old| earlier(window.first, old)) {
            self.first = Some(window.first);
        }
        if self.end.is_none_or(|old| earlier(old, window.end)) {
            self.end = Some(window.end);
        }
        Ok(())
    }
    pub(super) fn check_origin(&self) -> Result<(), ConversionError> {
        if !equal(
            self.origin.ok_or(error("dash-empty-segment-timeline"))?,
            self.first.ok_or(error("dash-empty-segment-timeline"))?,
        ) {
            return Err(error("dash-presentation-origin-mismatch"));
        }
        Ok(())
    }
    fn nanos(&self) -> Result<u64, ConversionError> {
        let bad = error("dash-presentation-duration-limit");
        let (end, scale) = self.end.ok_or(bad)?;
        let (origin, origin_scale) = self.origin.ok_or(bad)?;
        let numerator = (u128::from(end) * u128::from(origin_scale))
            .checked_sub(u128::from(origin) * u128::from(scale))
            .ok_or(bad)?;
        if numerator == 0 {
            return Err(bad);
        }
        let denominator = u128::from(scale) * u128::from(origin_scale);
        // Only the finite XML period-end decimal is rounded upward. Sample and packet clocks stay exact.
        u64::try_from(
            numerator
                .checked_mul(u128::from(NANOS))
                .ok_or(bad)?
                .div_ceil(denominator),
        )
        .map_err(|_| bad)
    }
    pub(super) fn check_duration(&self, value: &str) -> Result<(), ConversionError> {
        let (n, d) = decimal_duration(value)?;
        if u128::from(n) * u128::from(NANOS) != u128::from(self.nanos()?) * u128::from(d) {
            return Err(error("dash-presentation-duration-mismatch"));
        }
        Ok(())
    }
    fn text(&self) -> Result<String, ConversionError> {
        let n = self.nanos()?;
        Ok(format!("PT{}.{:09}S", n / NANOS, n % NANOS))
    }
    fn offset(&self, scale: u64) -> Result<u64, ConversionError> {
        let (n, d) = self.first.ok_or(error("dash-empty-segment-timeline"))?;
        let n = u128::from(n) * u128::from(scale);
        if n % u128::from(d) != 0 {
            return Err(error("dash-presentation-origin-timescale-not-supported"));
        }
        u64::try_from(n / u128::from(d)).map_err(|_| error("dash-presentation-duration-limit"))
    }
}
fn observed_window(
    rep: Node<'_, '_>,
    directory: &Path,
    remaining: usize,
) -> Result<(Window, usize), ConversionError> {
    let (base, templates) = representation_scopes(rep)?;
    let values = sequence(&templates, remaining)?;
    let id = rep.attribute("id").unwrap_or("");
    let bandwidth = rep.attribute("bandwidth").unwrap_or("");
    let init =
        attribute(&templates, "initialization").ok_or(error("dash-initialization-required"))?;
    let media = attribute(&templates, "media").ok_or(error("dash-media-template-required"))?;
    let initialization = reference(
        directory,
        &base,
        &expand(init, id, bandwidth, values[0].0, values[0].1)?,
    )?;
    let mut clock = crate::conversion::timing::FragmentClock::read(&initialization)?;
    let scale = integer(attribute(&templates, "timescale"), 1)?;
    for &(number, time, duration) in &values {
        let path = reference(
            directory,
            &base,
            &expand(media, id, bandwidth, number, time)?,
        )?;
        clock.check_dash(&path, time, duration, scale)?;
    }
    Ok((
        Window::read(&templates, clock.presentation_window()?)?,
        values.len(),
    ))
}
/// Finalize only fresh TS DASH: use the earliest actual presented sample across all roles,
/// correcting a video-selected Packager origin that could otherwise hide initial AAC frames.
/// No sample bytes, decode/composition timestamps, or `SegmentTimeline` entries are changed.
pub(in crate::conversion) fn finish_transport(path: &Path) -> Result<(), ConversionError> {
    let bytes = crate::conversion::files::read_manifest(path)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| error("dash-invalid-xml"))?;
    let doc = roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            nodes_limit: 20_000,
            ..Default::default()
        },
    )
    .map_err(|_| error("dash-invalid-xml"))?;
    let root = doc.root_element();
    let period = one(root, "Period")?.ok_or(error("dash-single-period-required"))?;
    let directory = path
        .parent()
        .ok_or(error("output-staging-failed"))?
        .canonicalize()
        .map_err(|_| error("output-staging-failed"))?;
    let mut presentation = Presentation::default();
    let mut used = 0;
    let representations: Vec<_> = children(period, "AdaptationSet")
        .into_iter()
        .flat_map(|a| children(a, "Representation"))
        .collect();
    if representations.is_empty() || representations.len() > super::MAX_TRACKS {
        return Err(error("dash-track-limit-or-structure-not-supported"));
    }
    for &rep in &representations {
        let (window, count) = observed_window(rep, &directory, super::MAX_SEGMENTS - used)?;
        used += count;
        presentation.include(window)?;
    }
    presentation.origin = presentation.first;
    let duration = presentation.text()?;
    let mut edits = Vec::new();
    for rep in representations {
        let template = one(rep, "SegmentTemplate")?.ok_or(error("dash-media-template-required"))?;
        let scale = integer(template.attribute("timescale"), 1)?;
        let offset = presentation.offset(scale)?.to_string();
        if let Some(attr) = template.attribute_node("presentationTimeOffset") {
            edits.push((attr.range_value(), offset));
        } else {
            let at = template.range().start + "<SegmentTemplate".len();
            edits.push((at..at, format!(" presentationTimeOffset=\"{offset}\"")));
        }
    }
    let attr = root
        .attribute_node("mediaPresentationDuration")
        .ok_or(error("dash-presentation-duration-required"))?;
    edits.push((attr.range_value(), duration.clone()));
    if let Some(attr) = period.attribute_node("duration") {
        edits.push((attr.range_value(), duration));
    } else {
        let start = period.range().start;
        let at = text[start..].find('>').ok_or(error("dash-invalid-xml"))? + start;
        edits.push((at..at, format!(" duration=\"{duration}\"")));
    }
    edits.sort_by_key(|(r, _)| std::cmp::Reverse(r.start));
    let mut result = text.to_owned();
    for (range, value) in edits {
        result.replace_range(range, &value);
    }
    crate::conversion::private_file_mode(path)?;
    let mut file = crate::native::create_media_or_truncate(path)
        .map_err(|_| error("output-staging-failed"))?;
    file.write_all(result.as_bytes())
        .map_err(|_| error("output-staging-failed"))?;
    file.sync_all().map_err(|_| error("output-staging-failed"))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_period_end_and_common_original_origin_are_exact() {
        let mut p = Presentation::default();
        p.include(Window {
            origin: (126_000, 90_000),
            first: (126_000, 90_000),
            end: (1_028_400, 90_000),
        })
        .unwrap();
        p.check_origin().unwrap();
        assert_eq!(p.text().unwrap(), "PT10.026666667S");
        p.check_duration("PT10.026666667S").unwrap();
        for bad in ["PT10.026666666S", "PT10.026666668S", "PT10.026667S"] {
            assert_eq!(
                p.check_duration(bad).unwrap_err().0,
                "dash-presentation-duration-mismatch"
            );
        }
        for origin in [125_999, 126_001] {
            p.origin = Some((origin, 90_000));
            assert_eq!(
                p.check_origin().unwrap_err().0,
                "dash-presentation-origin-mismatch"
            );
        }
    }
    #[test]
    fn unequal_role_ends_and_timescales_keep_one_global_origin() {
        let mut p = Presentation::default();
        p.include(Window {
            origin: (126_000, 90_000),
            first: (127_920, 90_000),
            end: (1_027_920, 90_000),
        })
        .unwrap();
        p.include(Window {
            origin: (67_200, 48_000),
            first: (67_200, 48_000),
            end: (548_480, 48_000),
        })
        .unwrap();
        p.check_origin().unwrap();
        assert_eq!(p.text().unwrap(), "PT10.026666667S");
        assert_eq!(p.offset(90_000).unwrap(), 126_000);
        assert_eq!(p.offset(48_000).unwrap(), 67_200);
        assert_eq!(
            p.include(Window {
                origin: (126_001, 90_000),
                first: (126_000, 90_000),
                end: (1_028_400, 90_000)
            })
            .unwrap_err()
            .0,
            "dash-incoherent-presentation-offset"
        );
    }
}
