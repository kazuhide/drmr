use super::{ChildFact, HlsFacts, check, rule};
use crate::{CheckStatus, DiagnosticSink, NotCheckedReason, RuleScope, Severity};

pub(super) struct Graph<'a> {
    master: &'a HlsFacts,
    program_date_time: Option<bool>,
    date_ranges: Option<[u8; 32]>,
    dated: bool,
}

impl<'a> Graph<'a> {
    pub fn new(master: &'a HlsFacts) -> Self {
        Self {
            master,
            program_date_time: None,
            date_ranges: None,
            dated: false,
        }
    }

    pub fn check(&mut self, child: &ChildFact, actual: &HlsFacts, sink: &mut DiagnosticSink<'_>) {
        inherited(self.master, child, actual, sink);
        self.dated |= actual.program_date_time;
        check(
            sink,
            rule(
                "HLS-GRAPH-PDT-PRESENCE",
                "hls-program-date-time-presence-differs",
                "6.2.4",
            ),
            self.program_date_time
                .is_none_or(|previous| previous == actual.program_date_time),
            child.span,
        );
        self.program_date_time = Some(actual.program_date_time);
        self.ranges(child, actual, sink);
    }

    fn ranges(&mut self, child: &ChildFact, actual: &HlsFacts, sink: &mut DiagnosticSink<'_>) {
        let requirement = rule(
            "HLS-GRAPH-DATE-RANGES",
            "hls-child-date-range-sets-differ",
            "6.2.4",
        );
        match actual.date_ranges {
            Ok(current) => {
                check(
                    sink,
                    requirement,
                    self.date_ranges.is_none_or(|previous| previous == current),
                    child.span,
                );
                self.date_ranges.get_or_insert(current);
            }
            Err(reason) => sink.check(
                requirement,
                CheckStatus::NotChecked(reason),
                Some(child.span),
            ),
        }
    }

    pub fn finish(&self, complete: bool, sink: &mut DiagnosticSink<'_>) {
        if !complete {
            for (id, message, section) in [
                (
                    "HLS-GRAPH-PDT-PRESENCE",
                    "hls-program-date-time-presence-differs",
                    "6.2.4",
                ),
                (
                    "HLS-GRAPH-DATE-RANGES",
                    "hls-child-date-range-sets-differ",
                    "6.2.4",
                ),
                (
                    "HLS-GRAPH-TARGET-DURATIONS",
                    "hls-child-target-durations-differ",
                    "6.2.4",
                ),
                (
                    "HLS-GRAPH-PLAYLIST-TYPES",
                    "hls-child-playlist-types-differ",
                    "6.2.4",
                ),
            ] {
                sink.check(
                    rule(id, message, section),
                    CheckStatus::NotChecked(NotCheckedReason::MissingLinkedManifest),
                    None,
                );
            }
        }
        if self.dated {
            let mut requirement = rule(
                "HLS-GRAPH-PDT-MAPPING",
                "hls-program-date-time-media-mapping-not-inspected",
                "6.2.4",
            );
            requirement.scope = RuleScope::OutsideScope;
            sink.check(
                requirement,
                CheckStatus::NotChecked(NotCheckedReason::RequiresMediaEvidence),
                None,
            );
        }
    }
}

fn inherited(
    master: &HlsFacts,
    child: &ChildFact,
    actual: &HlsFacts,
    sink: &mut DiagnosticSink<'_>,
) {
    if let (Some(master), Some(media)) = (master.start, actual.start) {
        check(
            sink,
            rule("HLS-GRAPH-START", "hls-start-values-differ", "4.3.5"),
            master == media,
            child.span,
        );
        if master == media {
            let mut requirement = rule(
                "HLS-GRAPH-START-DUPLICATE",
                "hls-start-duplicated-in-child",
                "4.3.5",
            );
            requirement.severity = Severity::Warning;
            check(sink, requirement, false, child.span);
        }
    }
    if master.independent && actual.independent {
        let mut requirement = rule(
            "HLS-GRAPH-INDEPENDENT-DUPLICATE",
            "hls-independent-segments-duplicated-in-child",
            "4.3.5",
        );
        requirement.severity = Severity::Warning;
        check(sink, requirement, false, child.span);
    }
}
