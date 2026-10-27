//! Rebind generated DASH templates to the exact paths moved by source-layout placement.
use super::{
    ConversionError, Document, MAX_SEGMENTS, Path, PathBuf, addressing, children, error, one,
    policy,
};
use crate::{
    concurrency::Stop,
    conversion::{
        files,
        layout::{RelocatedTemplate, path_uri},
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

pub(in crate::conversion) fn relocate_output(
    root: &Path,
    files: &BTreeMap<PathBuf, PathBuf>,
    templates: &BTreeMap<PathBuf, RelocatedTemplate>,
    stop: &Stop,
) -> Result<(), ConversionError> {
    super::checkpoint(Some(stop))?;
    let manifest = root.join("manifest.mpd");
    let bytes = files::read_manifest(&manifest)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| error("dash-invalid-xml"))?;
    let document = Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            nodes_limit: 20_000,
            ..Default::default()
        },
    )
    .map_err(|_| error("dash-invalid-xml"))?;
    let period =
        one(document.root_element(), "Period")?.ok_or(error("dash-single-period-required"))?;
    let reps: Vec<_> = children(period, "AdaptationSet")
        .into_iter()
        .flat_map(|set| children(set, "Representation"))
        .collect();
    if reps.is_empty() || reps.len() > super::MAX_TRACKS || templates.len() != reps.len() {
        return Err(error("source-layout-track-count-mismatch"));
    }
    let mut remaining = MAX_SEGMENTS;
    let mut edits = Vec::new();
    let mut removals = BTreeSet::new();
    for rep in reps {
        super::checkpoint(Some(stop))?;
        let mut resolved = addressing::resolve(rep, remaining)?;
        remaining = remaining
            .checked_sub(resolved.segments.len())
            .ok_or(error("dash-segment-limit"))?;
        if resolved.kind != addressing::Kind::Template
            || resolved.nominal.is_some()
            || resolved.index.is_some()
        {
            return Err(error("source-layout-addressing-not-supported"));
        }
        let old_initialization = resolved
            .base
            .join(super::relative(&resolved.initialization.uri)?);
        let template = templates
            .get(&old_initialization)
            .ok_or(error("source-layout-resource-missing"))?;
        let node = addressing_node(rep, &mut resolved, files, template)?;
        policy::replace_addressing(text, rep, node, &mut edits, &mut removals)?;
    }
    edits.extend(
        removals
            .into_iter()
            .map(|(start, end)| (start..end, String::new())),
    );
    let output = policy::apply_edits(text, edits, stop)?;
    let referenced = super::source_references(output.as_bytes())?;
    let expected: BTreeSet<_> = super::source_references(text.as_bytes())?
        .into_iter()
        .map(|old| {
            files
                .get(&old)
                .cloned()
                .ok_or(error("source-layout-resource-missing"))
        })
        .collect::<Result<_, _>>()?;
    if referenced.into_iter().collect::<BTreeSet<_>>() != expected {
        return Err(error("source-layout-resource-mismatch"));
    }
    policy::write_manifest(&manifest, &output)
}

fn addressing_node(
    rep: super::Node<'_, '_>,
    resolved: &mut addressing::Representation,
    files: &BTreeMap<PathBuf, PathBuf>,
    template: &RelocatedTemplate,
) -> Result<String, ConversionError> {
    let mapped = |resource: &addressing::Resource| -> Result<&PathBuf, ConversionError> {
        let relative = resolved.base.join(super::relative(&resource.uri)?);
        files
            .get(&relative)
            .ok_or(error("source-layout-resource-missing"))
    };
    let init = mapped(&resolved.initialization)?;
    for (index, segment) in resolved.segments.iter().enumerate() {
        let Some(media) = template.media.as_deref() else {
            break;
        };
        let number = match template.start_number {
            Some(start) => start
                .checked_add(index as u64)
                .ok_or(error("source-layout-template-mismatch"))?,
            None => segment.number,
        };
        let expanded = super::expand(
            media,
            rep.attribute("id").unwrap_or(""),
            rep.attribute("bandwidth").unwrap_or(""),
            number,
            segment.time,
        )?;
        if &super::relative(&expanded)? != mapped(&segment.resource)? {
            return Err(error("source-layout-template-mismatch"));
        }
    }
    let prefix = policy::prefix(rep)?;
    let node = if let Some(media) = &template.media {
        if let Some(start) = template.start_number {
            for (index, segment) in resolved.segments.iter_mut().enumerate() {
                segment.number = start
                    .checked_add(index as u64)
                    .ok_or(error("source-layout-template-mismatch"))?;
            }
        }
        let mut node = policy::common("SegmentTemplate", resolved, &prefix);
        write!(
            node,
            " initialization=\"{}\" media=\"{}\">",
            policy::xml(&path_uri(init)?.replace('$', "$$")),
            policy::xml(media)
        )
        .map_err(|_| error("output-staging-failed"))?;
        policy::timeline(&mut node, &prefix, &resolved.segments)?;
        write!(node, "</{prefix}SegmentTemplate>").map_err(|_| error("output-staging-failed"))?;
        node
    } else {
        let mut node = policy::common("SegmentList", resolved, &prefix);
        write!(
            node,
            "><{prefix}Initialization sourceURL=\"{}\"/>",
            policy::xml(&path_uri(init)?)
        )
        .map_err(|_| error("output-staging-failed"))?;
        policy::timeline(&mut node, &prefix, &resolved.segments)?;
        for segment in &resolved.segments {
            write!(
                node,
                "<{prefix}SegmentURL media=\"{}\"/>",
                policy::xml(&path_uri(mapped(&segment.resource)?)?)
            )
            .map_err(|_| error("output-staging-failed"))?;
        }
        write!(node, "</{prefix}SegmentList>").map_err(|_| error("output-staging-failed"))?;
        node
    };
    Ok(node)
}
