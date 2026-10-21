//! Bounded remote DASH addressing projected into the ordinary confined local profile.
use super::{ConversionError, Resource, valid_url};
use crate::{concurrency::Stop, conversion::dash as local, source};
use roxmltree::{Document, Node};
use std::{collections::BTreeSet, fmt::Write as _, ops::Range};

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

fn addressing(template: Node<'_, '_>) -> Result<(), ConversionError> {
    if template.attributes().any(|attribute| {
        attribute.namespace().is_some()
            || !matches!(
                attribute.name(),
                "initialization"
                    | "media"
                    | "timescale"
                    | "startNumber"
                    | "presentationTimeOffset"
                    | "duration"
            )
    }) {
        return Err(INVALID);
    }
    for timeline in template.children().filter(Node::is_element) {
        if timeline.tag_name().name() != "SegmentTimeline"
            || timeline.tag_name().namespace() != template.tag_name().namespace()
            || timeline.attributes().len() != 0
        {
            return Err(INVALID);
        }
        for sample in timeline.children().filter(Node::is_element) {
            if sample.tag_name().name() != "S"
                || sample.tag_name().namespace() != template.tag_name().namespace()
                || sample.children().any(|node| node.is_element())
                || sample.attributes().any(|attribute| {
                    attribute.namespace().is_some() || !matches!(attribute.name(), "t" | "d" | "r")
                })
            {
                return Err(INVALID);
            }
        }
    }
    Ok(())
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

fn local_template(
    rep: Node<'_, '_>,
    templates: &[Node<'_, '_>],
    index: usize,
) -> Result<String, ConversionError> {
    let prefix = prefix(rep)?;
    let mut text = format!(
        "<{prefix}SegmentTemplate initialization=\"media/d{index:06}-init.mp4\" media=\"media/d{index:06}-$Number$.m4s\""
    );
    for name in [
        "timescale",
        "startNumber",
        "presentationTimeOffset",
        "duration",
    ] {
        if let Some(value) = local::attribute(templates, name) {
            let value = local::integer(Some(value), 0)?;
            write!(text, " {name}=\"{value}\"").expect("writing a string cannot fail");
        }
    }
    write!(text, "><{prefix}SegmentTimeline>").expect("writing a string cannot fail");
    for sample in local::children(local::timeline(templates)?, "S") {
        write!(text, "<{prefix}S").expect("writing a string cannot fail");
        for name in ["t", "d", "r"] {
            if let Some(value) = sample.attribute(name) {
                let value = local::integer(Some(value), 0)?;
                write!(text, " {name}=\"{value}\"").expect("writing a string cannot fail");
            }
        }
        text.push_str("/>");
        if text.len() > MAX_MANIFEST {
            return Err(LIMIT);
        }
    }
    write!(text, "</{prefix}SegmentTimeline></{prefix}SegmentTemplate>")
        .expect("writing a string cannot fail");
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

fn add_resource(
    resources: &mut Vec<Resource>,
    text_bytes: &mut usize,
    max_resources: usize,
    url: String,
    path: String,
) -> Result<(), ConversionError> {
    if resources.len() >= max_resources {
        return Err(LIMIT);
    }
    *text_bytes = text_bytes
        .checked_add(url.len())
        .and_then(|size| size.checked_add(path.len()))
        .filter(|size| *size <= MAX_REFERENCE_BYTES)
        .ok_or(LIMIT)?;
    resources.push(Resource { url, path });
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

/// Enumerate finite media URLs before workers start; no keys or license URLs become resources.
pub(super) fn plan(
    body: &[u8],
    effective: &str,
    stop: &Stop,
    max_resources: usize,
    max_segments_per_track: usize,
) -> Result<Plan, ConversionError> {
    cancelled(stop)?;
    valid_url(effective)?;
    if body.len() > MAX_MANIFEST {
        return Err(LIMIT);
    }
    let text = std::str::from_utf8(body)
        .map_err(|_| INVALID)?
        .trim_start_matches('\u{feff}');
    if text.contains("<!DOCTYPE") {
        return Err(INVALID);
    }
    let document = Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            nodes_limit: 20_000,
            ..Default::default()
        },
    )
    .map_err(|_| INVALID)?;
    let representations = representations(document.root_element())?;
    let mut resources = Vec::new();
    let mut reference_bytes = body.len().saturating_add(effective.len());
    let mut remaining = local::MAX_SEGMENTS;
    let mut removals = BTreeSet::new();
    let mut edits = Vec::new();
    let mut added = 0usize;
    for (index, rep) in representations.into_iter().enumerate() {
        cancelled(stop)?;
        let mut base = effective.to_owned();
        let mut templates = Vec::new();
        for scope in local::validated_scopes(rep)? {
            if let Some(node) = local::one(scope, "BaseURL")? {
                base = resolved(&base, &base_reference(node)?)?;
                let range = node.range();
                removals.insert((range.start, range.end));
            }
            if let Some(node) = local::one(scope, "SegmentTemplate")? {
                addressing(node)?;
                templates.push(node);
                let range = node.range();
                removals.insert((range.start, range.end));
            }
        }
        let values = local::sequence(&templates, remaining.min(max_segments_per_track))?;
        cancelled(stop)?;
        remaining -= values.len();
        let initialization = local::initialization_uri(rep, &templates, values[0])?;
        add_resource(
            &mut resources,
            &mut reference_bytes,
            max_resources,
            resolved(&base, &initialization)?,
            format!("media/d{index:06}-init.mp4"),
        )?;
        let media = local::attribute(&templates, "media").ok_or(INVALID)?;
        let id = rep.attribute("id").unwrap_or("");
        let bandwidth = rep.attribute("bandwidth").unwrap_or("");
        let mut seen = BTreeSet::new();
        for (number, time, _) in values {
            cancelled(stop)?;
            let expanded = local::expand(media, id, bandwidth, number, time)?;
            let url = resolved(&base, &expanded)?;
            if !seen.insert(url.clone()) {
                return Err(ConversionError("dash-repeated-resource-not-supported"));
            }
            add_resource(
                &mut resources,
                &mut reference_bytes,
                max_resources,
                url,
                format!("media/d{index:06}-{number}.m4s"),
            )?;
        }
        let template = local_template(rep, &templates, index)?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
            good.replace("<Period>", "<Period><SegmentList/>"),
            good.replace("<Period>", "<Period><SegmentBase/>"),
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
        for source in cases {
            let error = plan(source.as_bytes(), BASE, &Stop::new(), 100, 100)
                .err()
                .expect("unsupported DASH must fail");
            assert!(!format!("{error:?}").contains("secret"));
        }
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
}
