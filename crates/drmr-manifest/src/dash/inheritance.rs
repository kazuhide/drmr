use super::{
    NS, check, not_checked,
    numeric::{Parsed, signed, unsigned},
    span, template,
};
use crate::{DiagnosticSink, NotCheckedReason};
use roxmltree::Node;

const NAMES: [&str; 13] = [
    "timescale",
    "presentationTimeOffset",
    "duration",
    "startNumber",
    "endNumber",
    "media",
    "initialization",
    "index",
    "bitstreamSwitching",
    "eptDelta",
    "indexRange",
    "indexRangeExact",
    "presentationDuration",
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Template,
    List,
    Base,
}

#[derive(Clone, Copy)]
pub(super) struct EffectiveAddressing<'a, 'input> {
    pub node: Node<'a, 'input>,
    pub kind: Kind,
    values: [Option<&'a str>; 13],
    pub timeline: Option<Node<'a, 'input>>,
    pub initialization: Option<Node<'a, 'input>>,
    pub urls: Option<Node<'a, 'input>>,
    pub timescale: Option<u64>,
    pub offset: Option<u64>,
}
impl<'a, 'input> EffectiveAddressing<'a, 'input> {
    pub fn inherit(node: Node<'a, 'input>, inherited: Option<&Self>) -> Self {
        let kind = match node.tag_name().name() {
            "SegmentList" => Kind::List,
            "SegmentBase" => Kind::Base,
            _ => Kind::Template,
        };
        let inherited = inherited.filter(|old| old.kind == kind);
        let mut values = inherited.map_or([None; 13], |old| old.values);
        for (index, name) in NAMES.iter().enumerate() {
            if let Some(value) = node.attribute(*name) {
                values[index] = Some(value);
            }
        }
        let child = |name| node.children().find(|child| child.has_tag_name((NS, name)));
        let timeline = child("SegmentTimeline").or(inherited.and_then(|old| old.timeline));
        let initialization =
            child("Initialization").or(inherited.and_then(|old| old.initialization));
        let urls = child("SegmentURL")
            .map(|_| node)
            .or(inherited.and_then(|old| old.urls));
        let timescale = values[0].map_or(Some(1), |value| {
            unsigned(value, u64::from(u32::MAX)).filter(|value| *value > 0)
        });
        let offset = values[1].map_or(Some(0), |value| unsigned(value, u64::MAX));
        Self {
            node,
            kind,
            values,
            timeline,
            initialization,
            urls,
            timescale,
            offset,
        }
    }
    pub fn value(&self, name: &str) -> Option<&'a str> {
        NAMES
            .iter()
            .position(|candidate| *candidate == name)
            .and_then(|index| self.values[index])
    }
    pub fn check(&self, sink: &mut DiagnosticSink<'_>) {
        self.integers(sink);
        if self.kind == Kind::Template {
            for value in NAMES[5..9]
                .iter()
                .filter_map(|name| self.node.attribute(*name))
            {
                match template::validate(value) {
                    Parsed::Value(()) => check(sink, "DASH-TEMPLATE-TOKENS", true, span(self.node)),
                    Parsed::Invalid => check(sink, "DASH-TEMPLATE-TOKENS", false, span(self.node)),
                    Parsed::Unsupported => not_checked(
                        sink,
                        "DASH-TEMPLATE-TOKENS",
                        NotCheckedReason::UnsupportedFeature,
                        span(self.node),
                    ),
                }
            }
        }
        if let Some(value) = self.node.attribute("eptDelta") {
            match signed(value) {
                Parsed::Value(_) => check(sink, "DASH-NOMINAL-TIMING", true, span(self.node)),
                Parsed::Invalid => check(sink, "DASH-NOMINAL-TIMING", false, span(self.node)),
                Parsed::Unsupported => not_checked(
                    sink,
                    "DASH-NOMINAL-TIMING",
                    NotCheckedReason::ResourceLimit,
                    span(self.node),
                ),
            }
        }
        let unique = [
            "Initialization",
            "RepresentationIndex",
            "SegmentTimeline",
            "BitstreamSwitching",
        ]
        .iter()
        .all(|name| {
            self.node
                .children()
                .filter(|node| node.has_tag_name((NS, *name)))
                .take(2)
                .count()
                <= 1
        });
        check(
            sink,
            "DASH-SEGMENT-ADDRESSING",
            unique
                && (self.timeline.is_none()
                    || self.value("duration").is_none() && self.value("eptDelta").is_none()),
            span(self.node),
        );
    }
    fn integers(&self, sink: &mut DiagnosticSink<'_>) {
        let template = self.kind == Kind::Template;
        let timescale_id = if template {
            "DASH-TEMPLATE-TIMESCALE"
        } else {
            "DASH-ADDRESSING-TIMESCALE"
        };
        let integers_id = if template {
            "DASH-TEMPLATE-INTEGERS"
        } else {
            "DASH-ADDRESSING-INTEGERS"
        };
        let timescale = self
            .node
            .attribute("timescale")
            .map_or(Some(1), |value| unsigned(value, u64::from(u32::MAX)));
        check(
            sink,
            timescale_id,
            timescale.is_some_and(|value| value > 0),
            span(self.node),
        );
        for name in NAMES
            .iter()
            .take(5)
            .skip(1)
            .chain(std::iter::once(&"presentationDuration"))
        {
            if let Some(value) = self.node.attribute(*name) {
                let maximum = if *name == "duration" {
                    u64::from(u32::MAX)
                } else {
                    u64::MAX
                };
                check(
                    sink,
                    integers_id,
                    unsigned(value, maximum).is_some_and(|value| *name != "duration" || value > 0),
                    span(self.node),
                );
            }
        }
    }
    pub fn complete(&self) -> bool {
        match self.kind {
            Kind::Template => {
                self.value("media").is_some()
                    && (self.value("duration").is_some() || self.timeline.is_some())
            }
            Kind::List => {
                self.urls.is_some() && (self.value("duration").is_some() || self.timeline.is_some())
            }
            Kind::Base => self.initialization.is_some() && self.value("indexRange").is_some(),
        }
    }
}
