//! Bounded addressing facts shared by local admission, source inventory and output checks.
use super::{
    ConversionError, MAX_SEGMENTS, Node, PathBuf, attribute, children, decimal_duration, error,
    expand, integer, one, sequence, validated_scopes,
};
use std::collections::BTreeMap;

// Match source graph reference storage and its bounded native working set.
const MAX_REFERENCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESOLUTION_BYTES: usize = 128 * 1024 * 1024;

pub(in crate::conversion) struct Admission<'a> {
    references: usize,
    retained: usize,
    limit: ConversionError,
    charge: Option<&'a mut dyn FnMut(usize) -> Result<(), ConversionError>>,
}

impl Admission<'static> {
    pub(in crate::conversion) fn local() -> Self {
        Self {
            references: 0,
            retained: 0,
            limit: error("source-graph-limit"),
            charge: None,
        }
    }
}

impl<'a> Admission<'a> {
    pub(super) fn new(
        limit: ConversionError,
        charge: &'a mut dyn FnMut(usize) -> Result<(), ConversionError>,
    ) -> Self {
        Self {
            references: 0,
            retained: 0,
            limit,
            charge: Some(charge),
        }
    }

    pub(super) fn charge(&mut self, bytes: usize) -> Result<(), ConversionError> {
        let retained = self
            .retained
            .checked_add(bytes)
            .filter(|bytes| *bytes <= MAX_RESOLUTION_BYTES)
            .ok_or(self.limit)?;
        if let Some(charge) = self.charge.as_mut() {
            charge(bytes)?;
        }
        self.retained = retained;
        Ok(())
    }

    fn resource(&mut self, uri: &str) -> Result<(), ConversionError> {
        self.resources(uri, 1)
    }

    fn resources(&mut self, uri: &str, count: usize) -> Result<(), ConversionError> {
        let references = self
            .references
            .checked_add(uri.len().checked_mul(count).ok_or(self.limit)?)
            .filter(|bytes| *bytes <= MAX_REFERENCE_BYTES)
            .ok_or(self.limit)?;
        // Resource and decoded Claims keys coexist; include B-tree nodes and ranges.
        let retained = uri
            .len()
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(1024))
            .and_then(|bytes| bytes.checked_mul(count))
            .ok_or(self.limit)?;
        self.charge(retained)?;
        self.references = references;
        Ok(())
    }

    pub(super) fn index(&mut self, uri: &str, count: usize) -> Result<(), ConversionError> {
        self.charge(
            count
                .checked_mul(std::mem::size_of::<Segment>())
                .ok_or(self.limit)?,
        )?;
        self.resources(uri, count)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::conversion) enum Kind {
    Template,
    List,
    Base,
}

/// Inclusive byte bounds in an immutable original resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct ByteRange {
    pub start: u64,
    pub end: u64,
}
impl ByteRange {
    pub fn length(self) -> Result<u64, ConversionError> {
        self.end
            .checked_sub(self.start)
            .and_then(|n| n.checked_add(1))
            .ok_or(error("dash-invalid-byte-range"))
    }
}
pub(super) fn range(value: &str) -> Result<ByteRange, ConversionError> {
    let (a, b) = value
        .split_once('-')
        .ok_or(error("dash-invalid-byte-range"))?;
    if a.is_empty()
        || b.is_empty()
        || a.len() > 20
        || b.len() > 20
        || !a.bytes().all(|n| n.is_ascii_digit())
        || !b.bytes().all(|n| n.is_ascii_digit())
    {
        return Err(error("dash-invalid-byte-range"));
    }
    let value = ByteRange {
        start: integer(Some(a), 0)?,
        end: integer(Some(b), 0)?,
    };
    value.length()?;
    Ok(value)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct Resource {
    pub uri: String,
    pub range: Option<ByteRange>,
}
#[derive(Clone, Debug)]
pub(in crate::conversion) struct Segment {
    pub resource: Resource,
    pub number: u64,
    pub time: u64,
    pub duration: u64,
}
#[derive(Clone, Copy, Debug)]
pub(in crate::conversion) struct Nominal {
    pub duration: u64,
    pub period: (u64, u64),
    pub delta: i64,
    pub explicit_presentation: bool,
}
impl Nominal {
    pub(super) fn count(self, scale: u64) -> Result<usize, ConversionError> {
        if self.duration == 0 || scale == 0 || self.period.1 == 0 {
            return Err(error("dash-presentation-duration-limit"));
        }
        let numerator = i128::from(self.period.0)
            .checked_mul(i128::from(scale))
            .and_then(|n| {
                i128::from(self.delta)
                    .checked_mul(i128::from(self.period.1))
                    .and_then(|delta| n.checked_sub(delta))
            })
            .filter(|n| *n > 0)
            .ok_or(error("dash-presentation-duration-limit"))?;
        let denominator = i128::from(self.period.1)
            .checked_mul(i128::from(self.duration))
            .ok_or(error("dash-segment-limit"))?;
        usize::try_from(
            numerator
                .checked_add(denominator - 1)
                .ok_or(error("dash-segment-limit"))?
                / denominator,
        )
        .map_err(|_| error("dash-segment-limit"))
    }
}
pub(in crate::conversion) struct Representation {
    pub kind: Kind,
    pub base: PathBuf,
    pub initialization: Resource,
    pub segments: Vec<Segment>,
    pub timescale: u64,
    pub offset: u64,
    pub presentation_duration: Option<u64>,
    pub nominal: Option<Nominal>,
    pub index: Option<Resource>,
}

#[derive(Default)]
struct Claims(BTreeMap<String, Option<BTreeMap<u64, u64>>>);
impl Claims {
    fn add(&mut self, resource: &Resource) -> Result<(), ConversionError> {
        let uri = crate::source::pyurl::unquote(&resource.uri);
        let bad = error("dash-resource-ranges-overlap");
        if let Some(previous) = self.0.get_mut(&uri) {
            let ranges = previous.as_mut().ok_or(bad)?;
            let range = resource.range.ok_or(bad)?;
            if ranges
                .range(..=range.start)
                .next_back()
                .is_some_and(|(_, end)| *end >= range.start)
                || ranges
                    .range(range.start..)
                    .next()
                    .is_some_and(|(start, _)| *start <= range.end)
            {
                return Err(bad);
            }
            ranges.insert(range.start, range.end);
        } else {
            self.0.insert(
                uri,
                resource
                    .range
                    .map(|range| BTreeMap::from([(range.start, range.end)])),
            );
        }
        Ok(())
    }
}

pub(super) fn effective<'a>(
    rep: Node<'a, 'a>,
) -> Result<(PathBuf, Kind, Vec<Node<'a, 'a>>), ConversionError> {
    let (mut base, directory) = local_base(rep)?;
    if !directory {
        base.pop();
    }
    let (kind, nodes) = addressing_nodes(rep)?;
    Ok((base, kind, nodes))
}

fn local_base(rep: Node<'_, '_>) -> Result<(PathBuf, bool), ConversionError> {
    let mut base = PathBuf::new();
    let mut directory = true;
    for scope in validated_scopes(rep)? {
        if let Some(url) = one(scope, "BaseURL")? {
            let raw = url.text().unwrap_or("").trim();
            if !directory {
                base.pop();
            }
            base.push(super::relative(raw)?);
            directory = raw.ends_with('/') || raw == "." || raw.ends_with("/.");
        }
    }
    Ok((
        base.components()
            .filter(|component| matches!(component, std::path::Component::Normal(_)))
            .collect(),
        directory,
    ))
}

fn addressing_nodes<'a>(rep: Node<'a, 'a>) -> Result<(Kind, Vec<Node<'a, 'a>>), ConversionError> {
    let mut kind = None;
    let mut nodes = Vec::new();
    for scope in validated_scopes(rep)? {
        if let Some(url) = one(scope, "BaseURL")?
            && (url.attributes().len() != 0 || url.children().any(|child| child.is_element()))
        {
            return Err(error("dash-base-url-not-supported"));
        }
        for (name, current) in [
            ("SegmentTemplate", Kind::Template),
            ("SegmentList", Kind::List),
            ("SegmentBase", Kind::Base),
        ] {
            if let Some(node) = one(scope, name)? {
                validate(node, current)?;
                if kind.is_some_and(|old| old != current) {
                    nodes.clear();
                }
                kind = Some(current);
                nodes.push(node);
            }
        }
    }
    Ok((kind.ok_or(error("dash-addressing-required"))?, nodes))
}

fn validate(node: Node<'_, '_>, kind: Kind) -> Result<(), ConversionError> {
    let bad = error("dash-addressing-attribute-not-supported");
    for attribute in node.attributes() {
        let supported = matches!(
            attribute.name(),
            "timescale" | "presentationTimeOffset" | "presentationDuration"
        ) || match kind {
            Kind::Template => matches!(
                attribute.name(),
                "initialization" | "media" | "startNumber" | "endNumber" | "duration" | "eptDelta"
            ),
            Kind::List => matches!(
                attribute.name(),
                "startNumber" | "endNumber" | "duration" | "eptDelta"
            ),
            Kind::Base => matches!(attribute.name(), "indexRange" | "indexRangeExact"),
        };
        if attribute.namespace().is_some() || !supported {
            return Err(bad);
        }
    }
    if node
        .attribute("indexRangeExact")
        .is_some_and(|value| !matches!(value, "true" | "false" | "1" | "0"))
    {
        return Err(error("dash-index-range-boolean-invalid"));
    }
    for child in node.children().filter(Node::is_element) {
        if child.tag_name().namespace() != node.tag_name().namespace() {
            return Err(bad);
        }
        match (child.tag_name().name(), kind) {
            ("SegmentTimeline", Kind::Template | Kind::List) => {
                if child.attributes().len() != 0 {
                    return Err(bad);
                }
                for sample in child.children().filter(Node::is_element) {
                    if sample.tag_name().name() != "S"
                        || sample.tag_name().namespace() != child.tag_name().namespace()
                        || sample.children().any(|node| node.is_element())
                        || sample.attributes().any(|attribute| {
                            attribute.namespace().is_some()
                                || !matches!(attribute.name(), "t" | "d" | "r")
                        })
                    {
                        return Err(bad);
                    }
                }
            }
            ("Initialization", Kind::List | Kind::Base) => {
                if child.children().any(|node| node.is_element())
                    || child.attributes().any(|attribute| {
                        attribute.namespace().is_some()
                            || !matches!(attribute.name(), "sourceURL" | "range")
                    })
                {
                    return Err(bad);
                }
            }
            ("SegmentURL", Kind::List) => {
                if child.children().any(|node| node.is_element())
                    || child.attributes().any(|attribute| {
                        attribute.namespace().is_some()
                            || !matches!(attribute.name(), "media" | "mediaRange")
                    })
                {
                    return Err(bad);
                }
            }
            _ => return Err(bad),
        }
    }
    Ok(())
}
fn inherited_child<'a>(
    nodes: &[Node<'a, 'a>],
    name: &str,
) -> Result<Option<Node<'a, 'a>>, ConversionError> {
    for &node in nodes.iter().rev() {
        if let Some(child) = one(node, name)? {
            return Ok(Some(child));
        }
    }
    Ok(None)
}
pub(super) fn period(rep: Node<'_, '_>) -> Result<(u64, u64), ConversionError> {
    let p = rep
        .ancestors()
        .find(|n| n.is_element() && n.tag_name().name() == "Period")
        .ok_or(error("dash-single-period-required"))?;
    if p.attribute("start")
        .is_some_and(|v| !matches!(v, "PT0S" | "PT0.0S"))
    {
        return Err(error("dash-period-offset-not-supported"));
    }
    let root = p.parent_element().ok_or(error("dash-invalid-xml"))?;
    let result = p
        .attribute("duration")
        .or_else(|| root.attribute("mediaPresentationDuration"))
        .ok_or(error("dash-presentation-duration-required"))?;
    let value = decimal_duration(result)?;
    if value.0 == 0 {
        return Err(error("dash-presentation-duration-limit"));
    }
    Ok(value)
}
fn signed(value: Option<&str>) -> Result<i64, ConversionError> {
    value.map_or(Ok(0), |v| {
        v.parse().map_err(|_| error("dash-invalid-template-number"))
    })
}
fn presentation_duration(
    rep: Node<'_, '_>,
    nodes: &[Node<'_, '_>],
    scale: u64,
) -> Result<Option<u64>, ConversionError> {
    let Some(raw) = attribute(nodes, "presentationDuration") else {
        return Ok(None);
    };
    let own = integer(Some(raw), 0)?;
    let period = period(rep)?;
    if own == 0 || u128::from(own) * u128::from(period.1) > u128::from(period.0) * u128::from(scale)
    {
        return Err(error("dash-representation-duration-outside-period"));
    }
    Ok(Some(own))
}
type Sequence = Vec<(u64, u64, u64)>;
fn duration_sequence(
    rep: Node<'_, '_>,
    nodes: &[Node<'_, '_>],
    remaining: usize,
    count: Option<usize>,
) -> Result<(Sequence, Nominal), ConversionError> {
    let d = integer(attribute(nodes, "duration"), 0)?;
    let scale = integer(attribute(nodes, "timescale"), 1)?;
    if d == 0 || d > u64::from(u32::MAX) || scale == 0 {
        return Err(error("dash-timeline-gap-overlap-or-duration-not-supported"));
    }
    let delta = signed(attribute(nodes, "eptDelta"))?;
    let declared = presentation_duration(rep, nodes, scale)?;
    let period = declared.map_or_else(|| period(rep), |own| Ok((own, scale)))?;
    let nominal = Nominal {
        duration: d,
        period,
        delta,
        explicit_presentation: declared.is_some(),
    };
    let derived = nominal.count(scale)?;
    let first = integer(attribute(nodes, "startNumber"), 1)?;
    let numbered = attribute(nodes, "endNumber")
        .map(|raw| {
            integer(Some(raw), 0).and_then(|end| {
                end.checked_sub(first)
                    .and_then(|n| n.checked_add(1))
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or(error("dash-end-number-mismatch"))
            })
        })
        .transpose()?;
    if numbered
        .zip(count)
        .is_some_and(|(numbered, explicit)| numbered != explicit)
    {
        return Err(error("dash-segment-list-count-mismatch"));
    }
    let count = numbered.or(count).unwrap_or(derived);
    if count == 0 || count > remaining.min(MAX_SEGMENTS) {
        return Err(error("dash-segment-limit"));
    }
    let last_start = i128::from(delta)
        .checked_add(
            i128::try_from(count - 1)
                .map_err(|_| error("dash-segment-limit"))?
                .checked_mul(i128::from(d))
                .ok_or(error("dash-template-number-overflow"))?,
        )
        .ok_or(error("dash-template-number-overflow"))?;
    if last_start
        .checked_mul(i128::from(period.1))
        .is_none_or(|start| start >= i128::from(period.0) * i128::from(scale))
    {
        return Err(error("dash-nominal-segment-outside-presentation"));
    }
    let origin = integer(attribute(nodes, "presentationTimeOffset"), 0)?;
    let mut values = Vec::with_capacity(count);
    for n in 0..count {
        let n = u64::try_from(n).map_err(|_| error("dash-segment-limit"))?;
        values.push((
            first
                .checked_add(n)
                .ok_or(error("dash-template-number-overflow"))?,
            origin
                .checked_add(
                    n.checked_mul(d)
                        .ok_or(error("dash-template-number-overflow"))?,
                )
                .ok_or(error("dash-template-number-overflow"))?,
            d,
        ));
    }
    Ok((values, nominal))
}

pub(super) fn resolve(
    rep: Node<'_, '_>,
    remaining: usize,
) -> Result<Representation, ConversionError> {
    resolve_admitted(rep, remaining, &mut Admission::local())
}

pub(super) fn resolve_admitted(
    rep: Node<'_, '_>,
    remaining: usize,
    admission: &mut Admission<'_>,
) -> Result<Representation, ConversionError> {
    let (base, kind, nodes) = effective(rep)?;
    resolve_parts(rep, remaining, base, kind, &nodes, None, admission)
}

/// Remote acquisition resolves the URL hierarchy separately and confines its local projection.
/// Admit bounded numeric tables and each URI before retaining its Resource/Claims entry.
pub(in crate::conversion) fn resolve_remote(
    rep: Node<'_, '_>,
    remaining: usize,
    effective_base: &str,
    admit: &mut dyn FnMut(usize) -> Result<(), ConversionError>,
) -> Result<Representation, ConversionError> {
    let (kind, nodes) = addressing_nodes(rep)?;
    let mut admission = Admission::new(error("download-graph-limit"), admit);
    resolve_parts(
        rep,
        remaining,
        PathBuf::new(),
        kind,
        &nodes,
        Some(effective_base),
        &mut admission,
    )
}

fn resource(
    uri: &str,
    bytes: Option<&str>,
    admit: &mut Admission<'_>,
) -> Result<Resource, ConversionError> {
    if uri.is_empty() || uri.len() > 4096 || uri.chars().any(char::is_control) {
        return Err(error("dash-resource-reference-not-supported"));
    }
    let range = bytes.map(range).transpose()?;
    admit.resource(uri)?;
    Ok(Resource {
        uri: uri.into(),
        range,
    })
}

fn indexed(
    rep: Node<'_, '_>,
    nodes: &[Node<'_, '_>],
    timescale: u64,
    offset: u64,
    remote_base: Option<&str>,
    presentation_duration: Option<u64>,
    admit: &mut Admission<'_>,
) -> Result<Representation, ConversionError> {
    let initialization =
        inherited_child(nodes, "Initialization")?.ok_or(error("dash-initialization-required"))?;
    if initialization.attribute("sourceURL").is_some() {
        return Err(error("dash-indexed-layout-not-supported"));
    }
    let range = range(
        initialization
            .attribute("range")
            .ok_or(error("dash-initialization-range-required"))?,
    )?;
    if range.start != 0 {
        return Err(error("dash-indexed-layout-not-supported"));
    }
    let index = range_or_error(attribute(nodes, "indexRange"))?;
    if index.start <= range.end {
        return Err(error("dash-index-range-overlap"));
    }
    let bases = validated_scopes(rep)?
        .into_iter()
        .filter_map(|scope| one(scope, "BaseURL").transpose())
        .collect::<Result<Vec<_>, _>>()?;
    if bases.is_empty() {
        return Err(error("dash-indexed-layout-not-supported"));
    }
    let uri = if let Some(base) = remote_base {
        base.to_owned()
    } else {
        let (path, directory) = local_base(rep)?;
        if directory {
            return Err(error("dash-indexed-layout-not-supported"));
        }
        crate::conversion::layout::path_uri(&path)?
    };
    admit.resource(&uri)?;
    admit.resource(&uri)?;
    Ok(Representation {
        kind: Kind::Base,
        base: PathBuf::new(),
        initialization: Resource {
            uri: uri.clone(),
            range: Some(range),
        },
        segments: Vec::new(),
        timescale,
        offset,
        presentation_duration,
        nominal: None,
        index: Some(Resource {
            uri,
            range: Some(index),
        }),
    })
}

fn shared_object(
    rep: Node<'_, '_>,
    remote_base: Option<&str>,
) -> Result<Option<String>, ConversionError> {
    if let Some(base) = remote_base {
        let has_base = validated_scopes(rep)?
            .into_iter()
            .any(|scope| one(scope, "BaseURL").is_ok_and(|node| node.is_some()));
        let url =
            url::Url::parse(base).map_err(|_| error("dash-resource-reference-not-supported"))?;
        return Ok((has_base && !url.path().ends_with('/')).then(|| base.to_owned()));
    }
    let (base, directory) = local_base(rep)?;
    if directory {
        return Ok(None);
    }
    base.file_name()
        .map(|name| crate::conversion::layout::path_uri(std::path::Path::new(name)))
        .transpose()
}

fn initialization(
    rep: Node<'_, '_>,
    nodes: &[Node<'_, '_>],
    kind: Kind,
    first: (u64, u64, u64),
    shared: Option<&str>,
    admit: &mut Admission<'_>,
) -> Result<Resource, ConversionError> {
    if kind == Kind::Template {
        resource(
            &expand(
                attribute(nodes, "initialization").ok_or(error("dash-initialization-required"))?,
                rep.attribute("id").unwrap_or(""),
                rep.attribute("bandwidth").unwrap_or(""),
                first.0,
                first.1,
            )?,
            None,
            admit,
        )
    } else {
        let node = inherited_child(nodes, "Initialization")?
            .ok_or(error("dash-initialization-required"))?;
        if node.attribute("sourceURL").is_none() && node.attribute("range").is_none() {
            return Err(error("dash-initialization-required"));
        }
        resource(
            node.attribute("sourceURL")
                .or(shared)
                .ok_or(error("dash-initialization-required"))?,
            node.attribute("range"),
            admit,
        )
    }
}

fn table_bytes(
    sequence_capacity: usize,
    segment_count: usize,
    url_capacity: usize,
) -> Result<usize, ConversionError> {
    [
        (sequence_capacity, std::mem::size_of::<(u64, u64, u64)>()),
        (segment_count, std::mem::size_of::<Segment>()),
        (url_capacity, std::mem::size_of::<Node<'_, '_>>()),
    ]
    .into_iter()
    .try_fold(0usize, |bytes, (count, size)| {
        bytes.checked_add(count.checked_mul(size)?)
    })
    .ok_or(error("dash-segment-limit"))
}

fn segment_resource(
    rep: Node<'_, '_>,
    nodes: &[Node<'_, '_>],
    kind: Kind,
    position: (u64, u64),
    node: Option<Node<'_, '_>>,
    shared: Option<&str>,
    admit: &mut Admission<'_>,
) -> Result<Resource, ConversionError> {
    if kind == Kind::Template {
        resource(
            &expand(
                attribute(nodes, "media").ok_or(error("dash-media-template-required"))?,
                rep.attribute("id").unwrap_or(""),
                rep.attribute("bandwidth").unwrap_or(""),
                position.0,
                position.1,
            )?,
            None,
            admit,
        )
    } else {
        let node = node.ok_or(error("dash-segment-list-count-mismatch"))?;
        if node.attribute("media").is_none() && node.attribute("mediaRange").is_none() {
            return Err(error("dash-resource-reference-not-supported"));
        }
        if node.attribute("index").is_some() || node.attribute("indexRange").is_some() {
            return Err(error("dash-segment-index-not-supported"));
        }
        resource(
            node.attribute("media")
                .or(shared)
                .ok_or(error("dash-resource-reference-not-supported"))?,
            node.attribute("mediaRange"),
            admit,
        )
    }
}

fn resolve_parts(
    rep: Node<'_, '_>,
    remaining: usize,
    base: PathBuf,
    kind: Kind,
    nodes: &[Node<'_, '_>],
    remote_base: Option<&str>,
    admit: &mut Admission<'_>,
) -> Result<Representation, ConversionError> {
    let timescale = integer(attribute(nodes, "timescale"), 1)?;
    let offset = integer(attribute(nodes, "presentationTimeOffset"), 0)?;
    if timescale == 0 || timescale > u64::from(u32::MAX) {
        return Err(error("dash-presentation-offset-or-timescale-not-supported"));
    }
    let own = presentation_duration(rep, nodes, timescale)?;
    if kind == Kind::Base {
        return indexed(rep, nodes, timescale, offset, remote_base, own, admit);
    }
    let urls = if kind == Kind::List {
        nodes
            .iter()
            .rev()
            .map(|n| children(*n, "SegmentURL"))
            .find(|v| !v.is_empty())
            .ok_or(error("dash-empty-segment-list"))?
    } else {
        Vec::new()
    };
    let has_timeline = inherited_child(nodes, "SegmentTimeline")?.is_some();
    if has_timeline
        && (attribute(nodes, "duration").is_some() || attribute(nodes, "eptDelta").is_some())
    {
        return Err(error("dash-addressing-ambiguous"));
    }
    let (values, nominal) = if has_timeline {
        (sequence(nodes, remaining)?, None)
    } else {
        let (values, nominal) = duration_sequence(
            rep,
            nodes,
            remaining,
            (kind == Kind::List).then_some(urls.len()),
        )?;
        (values, Some(nominal))
    };
    if kind == Kind::List && urls.len() != values.len() {
        return Err(error("dash-segment-list-count-mismatch"));
    }
    if let Some(end) = attribute(nodes, "endNumber") {
        let end = integer(Some(end), 0)?;
        if values.last().is_none_or(|(number, _, _)| *number != end) {
            return Err(error("dash-end-number-mismatch"));
        }
    }
    let shared = if kind == Kind::List {
        shared_object(rep, remote_base)?
    } else {
        None
    };
    // Numeric sequences are bounded by MAX_SEGMENTS in transient scratch. Pay
    // their actual capacity and the segment array before expanding any URI list.
    admit.charge(table_bytes(
        values.capacity(),
        values.len(),
        urls.capacity(),
    )?)?;
    let initialization = initialization(rep, nodes, kind, values[0], shared.as_deref(), admit)?;
    let mut segments = Vec::with_capacity(values.len());
    let mut claims = Claims::default();
    claims.add(&initialization)?;
    for (ordinal, (number, time, duration)) in values.into_iter().enumerate() {
        let item = segment_resource(
            rep,
            nodes,
            kind,
            (number, time),
            urls.get(ordinal).copied(),
            shared.as_deref(),
            admit,
        )?;
        claims.add(&item)?;
        segments.push(Segment {
            resource: item,
            number,
            time,
            duration,
        });
    }
    Ok(Representation {
        kind,
        base,
        initialization,
        segments,
        timescale,
        offset,
        presentation_duration: own,
        nominal,
        index: None,
    })
}
fn range_or_error(value: Option<&str>) -> Result<ByteRange, ConversionError> {
    range(value.ok_or(error("dash-index-range-required"))?)
}

pub(super) fn template_uri(
    base: &std::path::Path,
    template: &str,
) -> Result<String, ConversionError> {
    if base.as_os_str().is_empty() {
        return Ok(template.into());
    }
    let encoded = crate::conversion::layout::path_uri(base)?;
    Ok(format!("{encoded}/{template}"))
}

/// Validate nominal addressing without substituting nominal values for actual sample clocks.
pub(super) fn check_nominal(
    segment: &Segment,
    nominal: Nominal,
    offset: u64,
    scale: u64,
    actual: (u64, u64, u64),
    ordinal: usize,
    count: usize,
) -> Result<(), ConversionError> {
    let bad = error("dash-nominal-timing-mismatch");
    if actual.0 >= actual.1 || actual.2 == 0 || scale == 0 || nominal.duration == 0 {
        return Err(bad);
    }
    let mul = |a: i128, b: i128| a.checked_mul(b).ok_or(bad);
    let unit_actual = mul(i128::from(scale), i128::from(nominal.period.1))?;
    let unit_declared = mul(i128::from(actual.2), i128::from(nominal.period.1))?;
    let origin = mul(i128::from(offset), unit_declared)?;
    let end = origin
        .checked_add(mul(
            mul(i128::from(nominal.period.0), i128::from(scale))?,
            i128::from(actual.2),
        )?)
        .ok_or(bad)?;
    let start = mul(
        i128::from(segment.time)
            .checked_add(i128::from(nominal.delta))
            .ok_or(bad)?,
        unit_declared,
    )?;
    let nominal_end = start
        .checked_add(mul(i128::from(nominal.duration), unit_declared)?)
        .ok_or(bad)?;
    let first = mul(i128::from(actual.0), unit_actual)?;
    let last = mul(i128::from(actual.1), unit_actual)?;
    let allowed = mul(i128::from(nominal.duration), unit_declared)?;
    for (declared, observed) in [(start.max(origin), first), (nominal_end.min(end), last)] {
        if observed
            .checked_sub(declared)
            .and_then(i128::checked_abs)
            .and_then(|v| v.checked_mul(2))
            .ok_or(bad)?
            > allowed
        {
            return Err(bad);
        }
    }
    if start >= end || nominal_end <= origin || ordinal == 0 && last <= origin {
        return Err(bad);
    }
    if ordinal + 1 == count && (first >= end || nominal.explicit_presentation && last != end) {
        return Err(bad);
    }
    Ok(())
}

#[cfg(test)]
mod expansion_tests {
    use super::*;
    use roxmltree::Document;

    #[test]
    fn shared_index_references_respect_the_exact_cumulative_byte_ceiling() {
        let uri = "a".repeat(2048);
        let mut admission = Admission::local();
        admission.index(&uri, 4096).unwrap();
        admission.index(&uri, 4096).unwrap();
        assert_eq!(admission.index(&uri, 1), Err(error("source-graph-limit")));
        assert_eq!(
            Admission::local().index(&uri, usize::MAX),
            Err(error("source-graph-limit"))
        );
    }

    #[test]
    fn local_resolution_bounds_cumulative_expanded_uri_storage() {
        for (prefix, expected) in [
            ("a".repeat(3900), Some(error("source-graph-limit"))),
            (String::from("short-"), None),
        ] {
            let source = format!(
                "<MPD mediaPresentationDuration='PT4300S'><Period><AdaptationSet><Representation id='v'><SegmentTemplate initialization='init.mp4' media='{prefix}$Number$.m4s' duration='1'/></Representation></AdaptationSet></Period></MPD>"
            );
            let document = Document::parse(&source).unwrap();
            let rep = document
                .descendants()
                .find(|node| node.has_tag_name("Representation"))
                .unwrap();
            assert_eq!(resolve(rep, 100_000).err(), expected);
        }
    }
}
