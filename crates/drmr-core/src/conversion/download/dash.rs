//! Bounded remote DASH addressing projected into the ordinary confined local profile.
use super::{ConversionError, Resource, valid_url};
use crate::{
    concurrency::Stop,
    conversion::{
        dash as local,
        layout::{UrlPathMapper, path_text, path_uri},
    },
    source,
};
use roxmltree::{Document, Node};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    ops::Range,
};

const INVALID: ConversionError = ConversionError("download-source-not-supported");
const LIMIT: ConversionError = ConversionError("download-graph-limit");
const MAX_REFERENCE_BYTES: usize = 16 * 1024 * 1024;
// Match ordinary manifest inspection without narrowing a platform-sized integer.
const MAX_MANIFEST: usize = 4 * 1024 * 1024;

pub(super) struct Plan {
    pub(super) projection: Vec<u8>,
    pub(super) resources: Vec<Resource>,
}

fn cancelled(stop: &Stop) -> Result<(), ConversionError> {
    if stop.is_set() {
        Err(ConversionError("cancelled"))
    } else {
        Ok(())
    }
}

fn resolved(base: &str, reference: &str) -> Result<String, ConversionError> {
    if reference.is_empty() || reference.len() > 4096 || reference.chars().any(char::is_control) {
        return Err(INVALID);
    }
    let url = source::child_url(base, reference).map_err(|_| INVALID)?;
    valid_url(&url)?;
    Ok(url)
}

fn base_reference(node: Node<'_, '_>) -> Result<String, ConversionError> {
    if node.attributes().len() != 0 || node.children().any(|child| child.is_element()) {
        return Err(INVALID);
    }
    let mut text = String::new();
    for child in node.children().filter(Node::is_text) {
        let value = child.text().unwrap_or("");
        if text.len().saturating_add(value.len()) > 4096 {
            return Err(LIMIT);
        }
        text.push_str(value);
    }
    Ok(text.trim().into())
}

fn prefix(rep: Node<'_, '_>) -> Result<String, ConversionError> {
    match rep.tag_name().namespace() {
        None => Ok(String::new()),
        Some(local::DASH_NS) if rep.lookup_namespace_uri(None) == Some(local::DASH_NS) => {
            Ok(String::new())
        }
        Some(local::DASH_NS) => rep
            .lookup_prefix(local::DASH_NS)
            .filter(|value| !value.is_empty())
            .map(|value| format!("{value}:"))
            .ok_or(INVALID),
        _ => Err(INVALID),
    }
}

fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn local_addressing(
    rep: Node<'_, '_>,
    resolved: &local::addressing::Representation,
    initialization: &std::path::Path,
    media: &[std::path::PathBuf],
    normalized: bool,
    index: usize,
) -> Result<String, ConversionError> {
    use local::addressing::Kind;
    let prefix = prefix(rep)?;
    let init = xml(&path_uri(initialization)?);
    let own = resolved
        .presentation_duration
        .map_or_else(String::new, |duration| {
            format!(" presentationDuration=\"{duration}\"")
        });
    if resolved.kind == Kind::Base {
        let init_range = resolved.initialization.range.ok_or(INVALID)?;
        let index_range = resolved
            .index
            .as_ref()
            .and_then(|resource| resource.range)
            .ok_or(INVALID)?;
        return Ok(format!(
            "<{prefix}BaseURL>{init}</{prefix}BaseURL><{prefix}SegmentBase timescale=\"{}\" presentationTimeOffset=\"{}\"{own} indexRange=\"{}-{}\" indexRangeExact=\"true\"><{prefix}Initialization range=\"{}-{}\"/></{prefix}SegmentBase>",
            resolved.timescale,
            resolved.offset,
            index_range.start,
            index_range.end,
            init_range.start,
            init_range.end
        ));
    }
    let template = normalized && resolved.kind == Kind::Template;
    let name = if template {
        "SegmentTemplate"
    } else {
        "SegmentList"
    };
    let mut text = format!(
        "<{prefix}{name} timescale=\"{}\" presentationTimeOffset=\"{}\" startNumber=\"{}\" endNumber=\"{}\"{own}",
        resolved.timescale,
        resolved.offset,
        resolved.segments[0].number,
        resolved.segments.last().ok_or(INVALID)?.number
    );
    if template {
        write!(
            text,
            " initialization=\"{init}\" media=\"media/d{index:06}-$Number$.m4s\""
        )
        .map_err(|_| LIMIT)?;
    }
    if let Some(nominal) = resolved.nominal {
        write!(
            text,
            " duration=\"{}\" eptDelta=\"{}\"",
            nominal.duration, nominal.delta
        )
        .map_err(|_| LIMIT)?;
    }
    text.push('>');
    if !template {
        write!(text, "<{prefix}Initialization sourceURL=\"{init}\"").map_err(|_| LIMIT)?;
        if let Some(range) = resolved.initialization.range {
            write!(text, " range=\"{}-{}\"", range.start, range.end).map_err(|_| LIMIT)?;
        }
        text.push_str("/>");
    }
    if resolved.nominal.is_none() {
        write!(text, "<{prefix}SegmentTimeline>").map_err(|_| LIMIT)?;
        for segment in &resolved.segments {
            write!(
                text,
                "<{prefix}S t=\"{}\" d=\"{}\"/>",
                segment.time, segment.duration
            )
            .map_err(|_| LIMIT)?;
            if text.len() > MAX_MANIFEST {
                return Err(LIMIT);
            }
        }
        write!(text, "</{prefix}SegmentTimeline>").map_err(|_| LIMIT)?;
    }
    if !template {
        for (segment, path) in resolved.segments.iter().zip(media) {
            write!(
                text,
                "<{prefix}SegmentURL media=\"{}\"",
                xml(&path_uri(path)?)
            )
            .map_err(|_| LIMIT)?;
            if let Some(range) = segment.resource.range {
                write!(text, " mediaRange=\"{}-{}\"", range.start, range.end).map_err(|_| LIMIT)?;
            }
            text.push_str("/>");
            if text.len() > MAX_MANIFEST {
                return Err(LIMIT);
            }
        }
    }
    write!(text, "</{prefix}{name}>").map_err(|_| LIMIT)?;
    Ok(text)
}

fn insertion(
    source: &str,
    rep: Node<'_, '_>,
    template: String,
) -> Result<(Range<usize>, String), ConversionError> {
    let range = rep.range();
    let original = &source[range.clone()];
    if original.ends_with("/>") {
        let name = original
            .strip_prefix('<')
            .and_then(|value| value.split([' ', '\n', '\r', '\t', '/', '>']).next())
            .filter(|name| !name.is_empty())
            .ok_or(INVALID)?;
        Ok((range.end - 2..range.end, format!(">{template}</{name}>")))
    } else {
        let at = original.rfind("</").ok_or(INVALID)? + range.start;
        Ok((at..at, template))
    }
}

#[cfg(test)]
thread_local! {
    static RESOURCE_COMPARISONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn compare_resource_text(left: &str, right: &str) -> std::cmp::Ordering {
    #[cfg(test)]
    RESOURCE_COMPARISONS.with(|count| count.set(count.get() + 1));
    left.cmp(right)
}

#[derive(PartialEq, Eq)]
struct ResourceKey(String);

impl Ord for ResourceKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        compare_resource_text(&self.0, &other.0)
    }
}

impl PartialOrd for ResourceKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Default)]
struct ResourceIndex {
    urls: BTreeMap<ResourceKey, usize>,
    paths: BTreeSet<ResourceKey>,
}

fn add_resource(
    resources: &mut Vec<Resource>,
    index: &mut ResourceIndex,
    text_bytes: &mut usize,
    max_resources: usize,
    url: String,
    path: String,
    charge: &mut dyn FnMut(usize) -> Result<(), ConversionError>,
) -> Result<(), ConversionError> {
    let url = ResourceKey(url);
    let path = ResourceKey(path);
    if let Some(previous) = index.urls.get(&url) {
        return if compare_resource_text(&resources[*previous].path, &path.0).is_eq() {
            Ok(())
        } else {
            Err(INVALID)
        };
    }
    if index.paths.contains(&path) {
        return Err(INVALID);
    }
    if resources.len() >= max_resources {
        return Err(LIMIT);
    }
    let bytes = text_bytes
        .checked_add(url.0.len())
        .and_then(|size| size.checked_add(path.0.len()))
        .filter(|size| *size <= MAX_REFERENCE_BYTES)
        .ok_or(LIMIT)?;
    // Both lookup keys coexist with the ordered Resource strings. Include
    // B-tree occupancy and spare/relocating capacity in the ordered Vec.
    charge(
        url.0
            .capacity()
            .checked_add(path.0.capacity())
            .and_then(|bytes| bytes.checked_add(1024))
            .ok_or(LIMIT)?,
    )?;
    resources.push(Resource {
        url: url.0.clone(),
        path: path.0.clone(),
    });
    index.urls.insert(url, resources.len() - 1);
    index.paths.insert(path);
    *text_bytes = bytes;
    Ok(())
}

fn representations<'a>(root: Node<'a, 'a>) -> Result<Vec<Node<'a, 'a>>, ConversionError> {
    if root.tag_name().name() != "MPD"
        || !matches!(root.tag_name().namespace(), None | Some(local::DASH_NS))
        || root.attribute("type").unwrap_or("static") != "static"
    {
        return Err(INVALID);
    }
    let period = local::one(root, "Period")?.ok_or(INVALID)?;
    if period
        .attribute("start")
        .is_some_and(|start| !matches!(start, "PT0S" | "PT0.0S"))
    {
        return Err(ConversionError("dash-period-offset-not-supported"));
    }
    let durations = [
        root.attribute("mediaPresentationDuration"),
        period.attribute("duration"),
    ];
    if durations.iter().all(Option::is_none) {
        return Err(ConversionError("dash-presentation-duration-required"));
    }
    for duration in durations.into_iter().flatten() {
        local::decimal_duration(duration)?;
    }
    let representations: Vec<_> = local::children(period, "AdaptationSet")
        .into_iter()
        .flat_map(|adaptation| local::children(adaptation, "Representation"))
        .collect();
    if representations.is_empty()
        || representations.len() > local::MAX_TRACKS
        || representations.len()
            != root
                .descendants()
                .filter(|node| node.is_element() && node.tag_name().name() == "Representation")
                .count()
    {
        return Err(INVALID);
    }
    Ok(representations)
}

fn finish_projection(
    text: &str,
    mut edits: Vec<(Range<usize>, String)>,
    resources: &[Resource],
    stop: &Stop,
) -> Result<Vec<u8>, ConversionError> {
    let mut size = text.len();
    for (range, value) in &edits {
        size = size.checked_sub(range.len()).ok_or(INVALID)?;
        size = size.checked_add(value.len()).ok_or(LIMIT)?;
    }
    if size > MAX_MANIFEST {
        return Err(LIMIT);
    }
    edits.sort_by_key(|(range, _)| std::cmp::Reverse((range.start, range.end)));
    let mut projection = text.to_owned();
    for (range, value) in edits {
        cancelled(stop)?;
        projection.replace_range(range, &value);
    }
    // Canonicalization can replicate inherited timelines. Reapply the ordinary
    // local graph parser before fetching, including its projected XML-node cap.
    let references = local::source_references(projection.as_bytes())?;
    if references.len() != resources.len() {
        return Err(INVALID);
    }
    for (reference, resource) in references.iter().zip(resources) {
        cancelled(stop)?;
        if reference != std::path::Path::new(&resource.path) {
            return Err(INVALID);
        }
    }
    Ok(projection.into_bytes())
}

fn parse_manifest(body: &[u8]) -> Result<Document<'_>, ConversionError> {
    if body.len() > MAX_MANIFEST {
        return Err(LIMIT);
    }
    let text = std::str::from_utf8(body)
        .map_err(|_| INVALID)?
        .trim_start_matches('\u{feff}');
    if text.contains("<!DOCTYPE") {
        return Err(INVALID);
    }
    Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            nodes_limit: 20_000,
            ..Default::default()
        },
    )
    .map_err(|_| INVALID)
}

fn effective_base(
    rep: Node<'_, '_>,
    effective: &str,
    removals: &mut BTreeSet<(usize, usize)>,
) -> Result<String, ConversionError> {
    let mut base = effective.to_owned();
    for scope in local::validated_scopes(rep)? {
        if let Some(node) = local::one(scope, "BaseURL")? {
            base = resolved(&base, &base_reference(node)?)?;
            let range = node.range();
            removals.insert((range.start, range.end));
        }
        for name in ["SegmentTemplate", "SegmentList", "SegmentBase"] {
            if let Some(node) = local::one(scope, name)? {
                let range = node.range();
                removals.insert((range.start, range.end));
            }
        }
    }
    Ok(base)
}

fn resolve_representation(
    rep: Node<'_, '_>,
    remaining: usize,
    base: &str,
    stop: &Stop,
    charge: &mut dyn FnMut(usize) -> Result<(), ConversionError>,
) -> Result<local::addressing::Representation, ConversionError> {
    local::addressing::resolve_remote(rep, remaining, base, &mut |bytes| {
        cancelled(stop)?;
        charge(bytes)
    })
}

/// Enumerate finite media URLs before workers start; no keys or license URLs become resources.
pub(super) fn plan(
    body: &[u8],
    effective: &str,
    stop: &Stop,
    max_resources: usize,
    max_segments_per_track: usize,
    mapper: &mut UrlPathMapper,
    charge: &mut dyn FnMut(usize) -> Result<(), ConversionError>,
) -> Result<Plan, ConversionError> {
    cancelled(stop)?;
    valid_url(effective)?;
    let document = parse_manifest(body)?;
    let text = document.input_text();
    let representations = representations(document.root_element())?;
    let mut resources = Vec::new();
    let mut resource_index = ResourceIndex::default();
    let mut reference_bytes = body.len().saturating_add(effective.len());
    let mut remaining = local::MAX_SEGMENTS;
    let mut removals = BTreeSet::new();
    let mut edits = Vec::new();
    let mut added = 0usize;
    for (index, rep) in representations.into_iter().enumerate() {
        cancelled(stop)?;
        let base = effective_base(rep, effective, &mut removals)?;
        let facts = resolve_representation(
            rep,
            remaining.min(max_segments_per_track),
            &base,
            stop,
            charge,
        )?;
        cancelled(stop)?;
        remaining = remaining.checked_sub(facts.segments.len()).ok_or(LIMIT)?;
        let init_url = if facts.kind == local::addressing::Kind::Base {
            base.clone()
        } else {
            resolved(&base, &facts.initialization.uri)?
        };
        let initialization = map_resource(
            mapper,
            &init_url,
            &format!("media/d{index:06}-init.mp4"),
            charge,
        )?;
        add_resource(
            &mut resources,
            &mut resource_index,
            &mut reference_bytes,
            max_resources,
            init_url,
            path_text(&initialization)?,
            charge,
        )?;
        let mut paths = Vec::with_capacity(facts.segments.len());
        let mut seen = BTreeSet::new();
        for segment in &facts.segments {
            cancelled(stop)?;
            let url = resolved(&base, &segment.resource.uri)?;
            if !seen.insert((
                url.clone(),
                segment.resource.range.map(|range| (range.start, range.end)),
            )) {
                return Err(ConversionError("dash-repeated-resource-not-supported"));
            }
            let fallback = format!("media/d{index:06}-{}.m4s", segment.number);
            let path = map_resource(mapper, &url, &fallback, charge)?;
            add_resource(
                &mut resources,
                &mut resource_index,
                &mut reference_bytes,
                max_resources,
                url,
                path_text(&path)?,
                charge,
            )?;
            paths.push(path);
        }
        let normalized = initialization
            == std::path::Path::new(&format!("media/d{index:06}-init.mp4"))
            && paths.iter().zip(&facts.segments).all(|(path, segment)| {
                path == std::path::Path::new(&format!("media/d{index:06}-{}.m4s", segment.number))
            });
        let template = local_addressing(rep, &facts, &initialization, &paths, normalized, index)?;
        let edit = insertion(text, rep, template)?;
        added = added
            .checked_add(edit.1.len())
            .filter(|size| *size <= MAX_MANIFEST)
            .ok_or(LIMIT)?;
        edits.push(edit);
    }
    for (start, end) in removals {
        edits.push((start..end, String::new()));
    }
    let projection = finish_projection(text, edits, &resources, stop)?;
    Ok(Plan {
        projection,
        resources,
    })
}

fn map_resource(
    mapper: &mut UrlPathMapper,
    url: &str,
    fallback: &str,
    charge: &mut dyn FnMut(usize) -> Result<(), ConversionError>,
) -> Result<std::path::PathBuf, ConversionError> {
    let before = mapper.retained_bytes();
    let path = mapper.map_admitted(url, fallback, charge)?;
    let retained = mapper.retained_bytes().checked_sub(before).ok_or(LIMIT)?;
    let graph = if retained == 0 {
        0
    } else {
        url.len()
            .checked_mul(8)
            .and_then(|n| n.checked_add(1024))
            .ok_or(LIMIT)?
    };
    charge(
        graph
            .checked_add(path.capacity().checked_mul(4).ok_or(LIMIT)?)
            .and_then(|n| n.checked_add(128))
            .ok_or(LIMIT)?,
    )?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversion::layout::LayoutMode;

    fn plan(
        body: &[u8],
        effective: &str,
        stop: &Stop,
        max_resources: usize,
        max_segments_per_track: usize,
    ) -> Result<Plan, ConversionError> {
        let mut mapper = UrlPathMapper::new(effective, LayoutMode::Normalized)?;
        super::plan(
            body,
            effective,
            stop,
            max_resources,
            max_segments_per_track,
            &mut mapper,
            &mut |_| Ok(()),
        )
    }

    const BASE: &str = "https://cdn.example.test/redirected/path/manifest.mpd?auth=manifest";

    fn presentation(body: &str) -> String {
        format!(
            "<MPD type=\"static\" mediaPresentationDuration=\"PT4S\"><Period><AdaptationSet mimeType=\"video/mp4\">{body}</AdaptationSet></Period></MPD>"
        )
    }

    fn track(id: &str, media: &str) -> String {
        format!(
            "<Representation id=\"{id}\"><SegmentTemplate initialization=\"init-$RepresentationID$.mp4\" media=\"{media}\" timescale=\"1\" startNumber=\"7\"><SegmentTimeline><S t=\"10\" d=\"2\" r=\"1\"/></SegmentTimeline></SegmentTemplate></Representation>"
        )
    }

    fn urls(plan: &Plan) -> Vec<&str> {
        plan.resources
            .iter()
            .map(|resource| resource.url.as_str())
            .collect()
    }

    fn parse(body: &str) -> Plan {
        plan(body.as_bytes(), BASE, &Stop::new(), 100, 100).expect("finite DASH should localize")
    }

    #[test]
    fn inherited_bases_templates_number_and_time_use_the_effective_manifest_url() {
        let source = r#"<MPD type="static" mediaPresentationDuration="PT4S"><BaseURL>../assets/</BaseURL><Period><BaseURL>period/</BaseURL><SegmentTemplate timescale="1000" startNumber="7" presentationTimeOffset="10000" initialization="init-$RepresentationID$.mp4" media="s-$Bandwidth$-$Number%05d$-$Time$.m4s?token=a&amp;b=c"><SegmentTimeline><S t="10000" d="2000" r="1"/></SegmentTimeline></SegmentTemplate><AdaptationSet mimeType="video/mp4"><BaseURL>video/</BaseURL><Representation id="v0" bandwidth="500"><BaseURL>../selected/</BaseURL><SegmentTemplate initialization="header.mp4?token=init"/></Representation></AdaptationSet></Period></MPD>"#;
        let result = parse(source);
        assert_eq!(
            urls(&result),
            [
                "https://cdn.example.test/redirected/assets/period/selected/header.mp4?token=init",
                "https://cdn.example.test/redirected/assets/period/selected/s-500-00007-10000.m4s?token=a&b=c",
                "https://cdn.example.test/redirected/assets/period/selected/s-500-00008-12000.m4s?token=a&b=c",
            ]
        );
        assert_eq!(result.resources[2].path, "media/d000000-8.m4s");
        let projected = std::str::from_utf8(&result.projection).unwrap();
        assert!(!projected.contains("BaseURL"));
        assert!(!projected.contains("token="));
        assert!(projected.contains("presentationTimeOffset=\"10000\""));
        assert_eq!(
            local::source_references(projected.as_bytes())
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn ciphertext_signaling_and_non_addressing_bytes_are_preserved() {
        let cp = "<!-- preserve formatting -->\n<ContentProtection schemeIdUri='urn:mpeg:dash:mp4protection:2011' value='cbcs' c:default_KID='00010203-0405-0607-0809-0a0b0c0d0e0f'/><ContentProtection schemeIdUri='urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed'><c:pssh>cHJpdmF0ZS1wcmVzZXJ2ZWQ=</c:pssh><LicenseURL>https://never-fetched.example.test/license?private=sentinel</LicenseURL></ContentProtection>\n";
        let source = presentation(&format!("{cp}{}", track("v", "s-$Number$.m4s")))
            .replace("<MPD ", "<MPD xmlns:c='urn:mpeg:cenc:2013' ");
        let result = parse(&source);
        let projected = std::str::from_utf8(&result.projection).unwrap();
        assert!(projected.contains(cp));
        assert_eq!(result.resources.len(), 3);
        assert!(urls(&result).iter().all(|url| !url.contains("pssh")));
    }

    #[test]
    fn prefixed_namespaces_and_self_closing_representations_are_localized() {
        let source = "\u{feff}<d:MPD xmlns:d='urn:mpeg:dash:schema:mpd:2011' type='static' mediaPresentationDuration='PT4S'><d:Period><d:AdaptationSet><d:SegmentTemplate initialization='init.mp4' media='s-$Time$.m4s' timescale='1'><d:SegmentTimeline><d:S d='2' r='1'/></d:SegmentTimeline></d:SegmentTemplate><d:Representation id='v'/></d:AdaptationSet></d:Period></d:MPD>";
        let result = parse(source);
        assert_eq!(
            urls(&result)[2],
            "https://cdn.example.test/redirected/path/s-2.m4s"
        );
        let projected = std::str::from_utf8(&result.projection).unwrap();
        assert!(projected.contains("<d:SegmentTemplate"));
        assert!(projected.contains("</d:Representation>"));
        assert_eq!(
            local::source_references(projected.as_bytes())
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn absolute_bases_and_escaped_path_components_are_kept_remote_only() {
        let source = presentation(&track("v", "a%20b/$Number$.m4s?auth=a&amp;b=c")).replace(
            "<Period>",
            "<BaseURL>https://media.example.test/</BaseURL><Period>",
        );
        let result = parse(&source);
        assert_eq!(
            urls(&result)[1],
            "https://media.example.test/a%20b/7.m4s?auth=a&b=c"
        );
        assert_eq!(result.resources[1].path, "media/d000000-7.m4s");
        assert!(
            !String::from_utf8(result.projection)
                .unwrap()
                .contains("auth=")
        );
    }

    #[test]
    fn unsupported_or_ambiguous_addressing_never_becomes_a_partial_plan() {
        let good = presentation(&track("v", "s-$Number$.m4s"));
        let cases = [
            good.replace("type=\"static\"", "type=\"dynamic\""),
            good.replace(" mediaPresentationDuration=\"PT4S\"", ""),
            good.replace("PT4S", "PT1H"),
            good.replace(
                "<SegmentTimeline><S t=\"10\" d=\"2\" r=\"1\"/></SegmentTimeline>",
                "",
            ),
            good.replace("</MPD>", "<Period/></MPD>"),
            good.replace("<Period>", "<Period start='PT1S'>"),
            good.replace("<Period>", "<Period xml:base='https://secret.invalid/'>"),
            good.replace(
                "<Period>",
                "<Period><BaseURL>a/</BaseURL><BaseURL>b/</BaseURL>",
            ),
            good.replace("<Period>", "<Period><BaseURL>file:///secret/</BaseURL>"),
            good.replace(
                "<Period>",
                "<Period><BaseURL>https://user:secret@example.test/</BaseURL>",
            ),
            good.replace("<Period>", "<Period><BaseURL><x/></BaseURL>"),
            good.replace(
                "<Period>",
                "<Period><EssentialProperty schemeIdUri='urn:example:semantic'/>",
            ),
            good.replace(
                "<SegmentTemplate ",
                "<SegmentTemplate index='index-$Number$.mp4' ",
            ),
            good.replace("r=\"1\"", "r=\"-1\""),
            good.replace("r=\"1\"", "r=\"100000\""),
            good.replace("s-$Number$.m4s", "one-repeated.m4s"),
            good.replace("s-$Number$.m4s", "s-$Unknown$.m4s"),
            good.replace(
                "<S t=\"10\" d=\"2\" r=\"1\"/>",
                "<S t=\"10\" d=\"2\"/><S t=\"13\" d=\"2\"/>",
            ),
            good.replace(
                "<Representation ",
                "<x:Representation xmlns:x='urn:foreign' ",
            )
            .replace("</Representation>", "</x:Representation>"),
            format!("<!DOCTYPE MPD [<!ENTITY external SYSTEM 'https://secret.invalid/'>]>{good}"),
        ];
        for (case, source) in cases.into_iter().enumerate() {
            let error = plan(source.as_bytes(), BASE, &Stop::new(), 100, 100)
                .err()
                .unwrap_or_else(|| panic!("unsupported DASH case {case} must fail"));
            assert!(!format!("{error:?}").contains("secret"));
        }
    }

    #[test]
    fn nearest_addressing_kind_replaces_an_unused_ancestor_kind() {
        let good = presentation(&track("v", "s-$Number$.m4s"));
        for ancestor in ["SegmentList", "SegmentBase"] {
            let result = parse(&good.replace("<Period>", &format!("<Period><{ancestor}/>")));
            assert_eq!(result.resources.len(), 3);
        }
    }

    #[test]
    fn source_layout_localizes_nominal_templates_lists_and_single_file_ranges() {
        let cases = [
            (
                "<SegmentTemplate timescale='1000' duration='2000' initialization='video/1080p/init.mp4' media='video/1080p/part-$Number$.m4s'/>",
                vec![
                    "video/1080p/init.mp4",
                    "video/1080p/part-1.m4s",
                    "video/1080p/part-2.m4s",
                ],
            ),
            (
                "<SegmentList timescale='1000'><Initialization sourceURL='audio/stream.mp4' range='0-99'/><SegmentTimeline><S d='2000' r='1'/></SegmentTimeline><SegmentURL media='audio/stream.mp4' mediaRange='100-199'/><SegmentURL media='audio/stream.mp4' mediaRange='200-299'/></SegmentList>",
                vec!["audio/stream.mp4"],
            ),
            (
                "<BaseURL>video/720p/movie.mp4</BaseURL><SegmentBase timescale='1000' indexRange='100-155'><Initialization range='0-99'/></SegmentBase>",
                vec!["video/720p/movie.mp4"],
            ),
            (
                "<BaseURL>audio/stream.mp4</BaseURL><SegmentList timescale='1000'><Initialization range='0-99'/><SegmentTimeline><S d='2000' r='1'/></SegmentTimeline><SegmentURL mediaRange='100-199'/><SegmentURL mediaRange='200-299'/></SegmentList>",
                vec!["audio/stream.mp4"],
            ),
        ];
        for (addressing, expected) in cases {
            let input = presentation(&format!(
                "<Representation id='v'>{addressing}</Representation>"
            ));
            let mut mapper = UrlPathMapper::new(BASE, LayoutMode::Source).unwrap();
            let output = super::plan(
                input.as_bytes(),
                BASE,
                &Stop::new(),
                100,
                100,
                &mut mapper,
                &mut |_| Ok(()),
            )
            .unwrap();
            assert_eq!(
                output
                    .resources
                    .iter()
                    .map(|resource| resource.path.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(
                local::source_references(&output.projection).unwrap(),
                expected
                    .iter()
                    .map(std::path::PathBuf::from)
                    .collect::<Vec<_>>()
            );
            assert!(
                !String::from_utf8(output.projection)
                    .unwrap()
                    .contains("auth=")
            );
        }
    }

    #[test]
    fn local_projection_retains_exact_nominal_own_end_and_explicit_zero_based_count() {
        let input = presentation(
            "<Representation id='v'><SegmentTemplate timescale='1000' duration='2000' presentationDuration='2001' startNumber='0' endNumber='0' initialization='video/init.mp4' media='video/part-$Number$.m4s'/></Representation>",
        );
        let output = parse(&input);
        assert_eq!(output.resources.len(), 2);
        let text = String::from_utf8(output.projection).unwrap();
        assert!(text.contains("presentationDuration=\"2001\"") && text.contains("endNumber=\"0\""));
        assert_eq!(local::source_references(text.as_bytes()).unwrap().len(), 2);
    }

    #[test]
    fn inherited_timeline_projection_cannot_exceed_the_local_xml_node_limit() {
        let timeline = "<S d='1'/>".repeat(7_000);
        let source = format!(
            "<MPD mediaPresentationDuration='PT7000S'><Period><SegmentTemplate initialization='init-$RepresentationID$.mp4' media='s-$RepresentationID$-$Number$.m4s'><SegmentTimeline>{timeline}</SegmentTimeline></SegmentTemplate><AdaptationSet><Representation id='a'/><Representation id='b'/><Representation id='c'/></AdaptationSet></Period></MPD>"
        );
        assert_eq!(
            plan(source.as_bytes(), BASE, &Stop::new(), 100_000, 100_000).err(),
            Some(ConversionError("dash-invalid-xml"))
        );
    }

    #[test]
    fn cancelled_and_resource_or_reference_expansion_limits_fail_before_fetching() {
        let source = presentation(&track("v", "s-$Number$.m4s"));
        let stop = Stop::new();
        stop.set();
        assert_eq!(
            plan(source.as_bytes(), BASE, &stop, 100, 100).err(),
            Some(ConversionError("cancelled"))
        );
        assert_eq!(
            plan(source.as_bytes(), BASE, &Stop::new(), 2, 100).err(),
            Some(LIMIT)
        );
        assert_eq!(
            plan(source.as_bytes(), BASE, &Stop::new(), 100, 1).err(),
            Some(ConversionError("dash-segment-limit"))
        );
        assert_eq!(parse(&source).resources.len(), 3);
        let large = source.replace("r=\"1\"", "r=\"5000\"").replace(
            "s-$Number$.m4s",
            &format!("{}$Number$.m4s", "a".repeat(3900)),
        );
        assert_eq!(
            plan(large.as_bytes(), BASE, &Stop::new(), 100_000, 100_000).err(),
            Some(LIMIT)
        );
        let oversized = format!("<!--{}-->{source}", "a".repeat(MAX_MANIFEST));
        assert_eq!(
            plan(oversized.as_bytes(), BASE, &Stop::new(), 100, 100).err(),
            Some(LIMIT)
        );
    }

    #[test]
    fn admission_refuses_before_the_resolver_builds_later_resource_claims() {
        let id = "i".repeat(128);
        let prefix = "a".repeat(4096 - id.len() - 1);
        let source = presentation(&format!(
            "<Representation id='{id}'><SegmentTemplate initialization='init.mp4' media='{prefix}$RepresentationID$$Number$' duration='2' startNumber='9'/></Representation>"
        ));
        let mut mapper = UrlPathMapper::new(BASE, LayoutMode::Normalized).unwrap();
        let mut admissions = 0;
        let result = super::plan(
            source.as_bytes(),
            BASE,
            &Stop::new(),
            100,
            100,
            &mut mapper,
            &mut |_| {
                admissions += 1;
                Err(LIMIT)
            },
        );
        assert_eq!(result.err(), Some(LIMIT));
        assert_eq!(admissions, 1);
        assert_eq!(mapper.retained_bytes(), 0);
    }

    #[test]
    fn long_finite_uri_expansion_stops_in_the_resolver_at_the_metadata_budget() {
        let prefix = "a".repeat(3900);
        let source = format!(
            "<MPD mediaPresentationDuration='PT512S'><Period><AdaptationSet><Representation id='v'><SegmentTemplate initialization='init.mp4' media='{prefix}$Number$.m4s' duration='1'/></Representation></AdaptationSet></Period></MPD>"
        );
        let mut mapper = UrlPathMapper::new(BASE, LayoutMode::Normalized).unwrap();
        let mut bytes = 0usize;
        let mut admissions = 0;
        let limit = 128 * 1024;
        let result = super::plan(
            source.as_bytes(),
            BASE,
            &Stop::new(),
            1000,
            1000,
            &mut mapper,
            &mut |additional| {
                admissions += 1;
                let next = bytes.checked_add(additional).ok_or(LIMIT)?;
                if next > limit {
                    return Err(LIMIT);
                }
                bytes = next;
                Ok(())
            },
        );
        assert_eq!(result.err(), Some(LIMIT));
        assert!(bytes <= limit);
        assert!(
            admissions > 2 && admissions < 32,
            "admissions: {admissions}"
        );
        assert_eq!(
            mapper.retained_bytes(),
            0,
            "no completed URI list was mapped"
        );
    }

    #[test]
    fn index_admission_precedes_retention_and_preserves_duplicate_claims() {
        let mut resources = Vec::new();
        let mut index = ResourceIndex::default();
        let mut bytes = 0;
        let url = "https://cdn.example.test/shared.mp4";
        let path = "media/shared.mp4";
        assert_eq!(
            add_resource(
                &mut resources,
                &mut index,
                &mut bytes,
                1,
                url.into(),
                path.into(),
                &mut |_| Err(LIMIT)
            ),
            Err(LIMIT)
        );
        assert!(resources.is_empty() && index.urls.is_empty() && index.paths.is_empty());
        assert_eq!(bytes, 0);
        let mut charged = 0;
        add_resource(
            &mut resources,
            &mut index,
            &mut bytes,
            1,
            url.into(),
            path.into(),
            &mut |value| {
                charged += value;
                Ok(())
            },
        )
        .unwrap();
        assert!(charged >= url.len() + path.len() + 1024);
        for (next_url, next_path, expected) in [
            (url, path, Ok(())),
            (url, "media/other.mp4", Err(INVALID)),
            ("https://cdn.example.test/other.mp4", path, Err(INVALID)),
            (
                "https://cdn.example.test/other.mp4",
                "media/other.mp4",
                Err(LIMIT),
            ),
        ] {
            assert_eq!(
                add_resource(
                    &mut resources,
                    &mut index,
                    &mut bytes,
                    1,
                    next_url.into(),
                    next_path.into(),
                    &mut |_| panic!("no new retained entry")
                ),
                expected
            );
        }
        assert_eq!(resources.len(), 1);
        assert_eq!((index.urls.len(), index.paths.len()), (1, 1));
        assert_eq!(bytes, url.len() + path.len());
    }

    #[test]
    fn ordered_resource_lookup_comparisons_grow_below_quadratic() {
        let comparisons = |count| {
            let mut resources = Vec::new();
            let mut resource_index = ResourceIndex::default();
            let mut bytes = 0;
            RESOURCE_COMPARISONS.with(|value| value.set(0));
            for index in 0..count {
                add_resource(
                    &mut resources,
                    &mut resource_index,
                    &mut bytes,
                    count,
                    format!("https://cdn.example.test/s-{index:06}.m4s"),
                    format!("media/s-{index:06}.m4s"),
                    &mut |_| Ok(()),
                )
                .unwrap();
            }
            assert_eq!(resources.len(), count);
            for (index, resource) in resources.iter().enumerate() {
                assert_eq!(resource.path, format!("media/s-{index:06}.m4s"));
            }
            RESOURCE_COMPARISONS.with(std::cell::Cell::get)
        };
        let small = comparisons(128);
        let large = comparisons(512);
        eprintln!("resource lookup comparisons: 128={small}, 512={large}");
        assert!(small > 128 && large > 512);
        assert!(
            large < small * 8,
            "comparisons grew from {small} to {large}"
        );
    }
}
