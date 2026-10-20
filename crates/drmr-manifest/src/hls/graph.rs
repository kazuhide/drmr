use super::{ChildFact, ChildRole, HlsFacts, HlsKind, PlaylistType, check, rule};
use crate::{
    CheckStatus, DiagnosticSink, Document, DocumentId, LintContext, NotCheckedReason, Profile,
};
use std::collections::{BTreeMap, BTreeSet};

type Fingerprint = [u8; 32];
type KeyIndex = BTreeMap<Fingerprint, BTreeMap<Fingerprint, BTreeSet<DocumentId>>>;

#[derive(Default)]
pub(crate) struct GraphFacts {
    keys: KeyIndex,
    unresolved_keys: BTreeMap<DocumentId, BTreeSet<NotCheckedReason>>,
}

impl GraphFacts {
    pub fn new(all: &BTreeMap<DocumentId, HlsFacts>) -> Self {
        let mut index = Self::default();
        for (id, document) in all {
            for key in &document.keys {
                if let Some(reason) = key.unresolved {
                    index.unresolved_keys.entry(*id).or_default().insert(reason);
                }
                index
                    .keys
                    .entry(key.uri)
                    .or_default()
                    .entry(key.matching)
                    .or_default()
                    .insert(*id);
            }
        }
        index
    }

    fn matches(
        &self,
        uri: &Fingerprint,
        matching: &Fingerprint,
        children: &BTreeSet<DocumentId>,
    ) -> bool {
        self.keys.get(uri).is_none_or(|values| {
            values
                .iter()
                .all(|(value, documents)| value == matching || documents.is_disjoint(children))
        })
    }
}

#[derive(Default)]
struct Alignment {
    target: Option<u64>,
    playlist_type: Option<PlaylistType>,
    seen: bool,
}

impl Alignment {
    fn check(&mut self, child: &ChildFact, actual: &HlsFacts, sink: &mut DiagnosticSink<'_>) {
        let requirement = rule(
            "HLS-GRAPH-PLAYLIST-TYPES",
            "hls-child-playlist-types-differ",
            "6.2.4",
        );
        check(
            sink,
            requirement,
            !self.seen || self.playlist_type == actual.playlist_type,
            child.span,
        );
        self.playlist_type = actual.playlist_type;
        self.seen = true;
        let exempt = actual.playlist_type == Some(PlaylistType::Vod)
            && (child.role == ChildRole::Subtitles || actual.iframe_only);
        if exempt {
            return;
        }
        let requirement = rule(
            "HLS-GRAPH-TARGET-DURATIONS",
            "hls-child-target-durations-differ",
            "6.2.4",
        );
        if let Some(duration) = actual.target_duration {
            check(
                sink,
                requirement,
                self.target.is_none_or(|target| target == duration),
                child.span,
            );
            self.target = Some(duration);
        } else {
            sink.check(
                requirement,
                CheckStatus::NotChecked(NotCheckedReason::InvalidPrerequisite),
                Some(child.span),
            );
        }
    }
}

fn child_kind(
    child: &ChildFact,
    all: &BTreeMap<DocumentId, HlsFacts>,
    sink: &mut DiagnosticSink<'_>,
) -> bool {
    let requirement = rule(
        "HLS-GRAPH-PLAYLIST-KIND",
        "hls-child-playlist-kind-invalid",
        "4.3.4.2-4.3.4.3",
    );
    let Some(actual) = child.target.and_then(|target| all.get(&target)) else {
        sink.check(
            requirement,
            CheckStatus::NotChecked(NotCheckedReason::MissingLinkedManifest),
            Some(child.span),
        );
        return false;
    };
    if actual.kind == HlsKind::Invalid {
        sink.check(
            requirement,
            CheckStatus::NotChecked(NotCheckedReason::InvalidPrerequisite),
            Some(child.span),
        );
        return false;
    }
    let valid =
        actual.kind == HlsKind::Media && (child.role != ChildRole::IFrame || actual.iframe_only);
    check(sink, requirement, valid, child.span);
    valid
}

fn session_keys(
    facts: &HlsFacts,
    graph: &GraphFacts,
    complete: bool,
    sink: &mut DiagnosticSink<'_>,
) {
    let children = facts
        .children
        .iter()
        .filter_map(|child| child.target)
        .collect::<BTreeSet<_>>();
    let unresolved = children
        .iter()
        .filter_map(|id| graph.unresolved_keys.get(id))
        .flat_map(|reasons| reasons.iter().copied())
        .collect::<BTreeSet<_>>();
    let mut outcomes = BTreeMap::new();
    for key in &facts.session_keys {
        let requirement = rule(
            "HLS-SESSION-KEY-MATCH",
            "hls-session-key-matching-invalid",
            "4.3.4.5",
        );
        if key.unresolved.is_some() || !unresolved.is_empty() {
            for reason in key.unresolved.into_iter().chain(unresolved.iter().copied()) {
                sink.check(requirement, CheckStatus::NotChecked(reason), Some(key.span));
            }
        } else if complete {
            let matches = *outcomes
                .entry((key.uri, key.matching))
                .or_insert_with(|| graph.matches(&key.uri, &key.matching, &children));
            check(sink, requirement, matches, key.span);
        } else {
            sink.check(
                requirement,
                CheckStatus::NotChecked(NotCheckedReason::MissingLinkedManifest),
                Some(key.span),
            );
        }
    }
}

pub(super) fn lint(
    _document: &Document<'_>,
    facts: &HlsFacts,
    all: &BTreeMap<DocumentId, HlsFacts>,
    graph: &GraphFacts,
    context: &LintContext<'_>,
    sink: &mut DiagnosticSink<'_>,
) {
    if facts.kind != HlsKind::Master {
        return;
    }
    let mut alignment = Alignment::default();
    let mut metadata = super::metadata::Graph::new(facts);
    let mut complete = true;
    for child in &facts.children {
        if child_kind(child, all, sink) {
            let actual = &all[&child.target.unwrap()];
            alignment.check(child, actual, sink);
            metadata.check(child, actual, sink);
        } else {
            complete = false;
        }
    }
    metadata.finish(complete, sink);
    session_keys(facts, graph, complete, sink);
    if context.options.profile == Profile::AppleVodManifest {
        let mut requirement = super::apple_rule(
            "APPLE-HLS-MEDIA-ALIGNMENT",
            "apple-hls-media-alignment-not-inspected",
            "8.1,8.3,8.7",
        );
        requirement.scope = crate::RuleScope::OutsideScope;
        sink.check(
            requirement,
            CheckStatus::NotChecked(NotCheckedReason::RequiresMediaEvidence),
            None,
        );
    }
}
