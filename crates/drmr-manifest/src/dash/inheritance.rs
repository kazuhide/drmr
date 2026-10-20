use super::{
    NS, check, not_checked,
    numeric::{Parsed, unsigned},
    span, template,
};
use crate::{DiagnosticSink, NotCheckedReason};
use roxmltree::Node;

const TEMPLATE_NAMES: [&str; 9] = [
    "timescale",
    "presentationTimeOffset",
    "duration",
    "startNumber",
    "endNumber",
    "media",
    "initialization",
    "index",
    "bitstreamSwitching",
];

#[derive(Clone, Copy)]
pub(super) struct EffectiveTemplate<'a, 'input> {
    node: Node<'a, 'input>,
    values: [Option<&'a str>; 9],
    pub timeline: Option<Node<'a, 'input>>,
    pub timescale: Option<u64>,
    pub offset: Option<u64>,
}
impl<'a, 'input> EffectiveTemplate<'a, 'input> {
    pub fn inherit(node: Node<'a, 'input>, inherited: Option<Self>) -> Self {
        let mut values = inherited.map_or([None; 9], |template| template.values);
        for (index, name) in TEMPLATE_NAMES.iter().enumerate() {
            if let Some(value) = node.attribute(*name) {
                values[index] = Some(value);
            }
        }
        let timeline = node
            .children()
            .find(|child| child.has_tag_name((NS, "SegmentTimeline")))
            .or(inherited.and_then(|template| template.timeline));
        let timescale = node.attribute("timescale").map_or_else(
            || inherited.map_or(Some(1), |template| template.timescale),
            |value| unsigned(value, u64::from(u32::MAX)).filter(|value| *value > 0),
        );
        let offset = node.attribute("presentationTimeOffset").map_or_else(
            || inherited.map_or(Some(0), |template| template.offset),
            |value| unsigned(value, u64::MAX),
        );
        Self {
            node,
            values,
            timeline,
            timescale,
            offset,
        }
    }
    pub fn check(self, sink: &mut DiagnosticSink<'_>) {
        // Validate each lexical declaration once. Inherited fields have already
        // been checked by their declaring template; cross-field checks below use
        // the immutable effective facts without rescanning inherited strings.
        let timescale = self
            .node
            .attribute("timescale")
            .map_or(Some(1), |value| unsigned(value, u64::from(u32::MAX)));
        check(
            sink,
            "DASH-TEMPLATE-TIMESCALE",
            timescale.is_some_and(|value| value > 0),
            span(self.node),
        );
        for name in TEMPLATE_NAMES.iter().take(5).skip(1) {
            if let Some(value) = self.node.attribute(*name) {
                let maximum = if *name == "presentationTimeOffset" {
                    u64::MAX
                } else {
                    u64::from(u32::MAX)
                };
                check(
                    sink,
                    "DASH-TEMPLATE-INTEGERS",
                    unsigned(value, maximum).is_some_and(|value| *name != "duration" || value > 0),
                    span(self.node),
                );
            }
        }
        for value in TEMPLATE_NAMES[5..]
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
        check(
            sink,
            "DASH-SEGMENT-ADDRESSING",
            self.values[2].is_none() || self.timeline.is_none(),
            span(self.node),
        );
    }
    pub fn complete(self) -> bool {
        self.values[5].is_some() && (self.values[2].is_some() || self.timeline.is_some())
    }
}
