use super::{
    NS, addressing, attributes, check, duration_check, inheritance::EffectiveAddressing,
    not_checked, numeric::unsigned, period, protection, span, terminal, timeline,
};
use crate::{DiagnosticSink, LintContext, NotCheckedReason};
use roxmltree::Node;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

#[derive(Clone, Copy)]
pub(super) enum Job<'a, 'input> {
    Element(Node<'a, 'input>),
    Representation {
        node: Node<'a, 'input>,
        template: Option<usize>,
        duration: terminal::PeriodDuration,
        timeline: Option<usize>,
        list: Option<usize>,
        base: addressing::BaseContext,
    },
    Template(usize),
    List(usize),
    Timeline(usize),
    Terminal(usize),
}
struct Timeline<'a, 'input> {
    node: Node<'a, 'input>,
    facts: OnceLock<timeline::Facts>,
}
struct Terminal {
    timeline: usize,
    duration: terminal::PeriodDuration,
    timescale: Option<u64>,
    offset: Option<u64>,
}
type ContextKey = (usize, Option<u32>, Option<u64>, Option<u64>);

#[derive(Default)]
struct Registrations {
    timelines: BTreeMap<u32, usize>,
    lists: BTreeMap<u32, usize>,
    contexts: BTreeSet<ContextKey>,
}

#[derive(Default)]
struct Scopes {
    ends: Vec<usize>,
    addressings: Vec<(usize, Option<usize>)>,
    periods: Vec<(usize, u32)>,
    bases: Vec<(usize, addressing::BaseContext)>,
}
impl Scopes {
    fn enter(&mut self, node: Node<'_, '_>, maximum: usize) -> bool {
        let range = node.range();
        while self.ends.last().is_some_and(|end| *end <= range.start) {
            self.ends.pop();
        }
        while self
            .addressings
            .last()
            .is_some_and(|(end, _)| *end <= range.start)
        {
            self.addressings.pop();
        }
        while self
            .periods
            .last()
            .is_some_and(|(end, _)| *end <= range.start)
        {
            self.periods.pop();
        }
        while self
            .bases
            .last()
            .is_some_and(|(end, _)| *end <= range.start)
        {
            self.bases.pop();
        }
        if self.ends.len() >= maximum {
            return false;
        }
        self.ends.push(range.end);
        true
    }
    fn base(&mut self, node: Node<'_, '_>) {
        if matches!(
            node.tag_name().name(),
            "MPD" | "Period" | "AdaptationSet" | "Representation"
        ) {
            let inherited = self
                .bases
                .last()
                .map_or_else(addressing::BaseContext::default, |(_, context)| *context);
            self.bases.push((
                node.range().end,
                addressing::BaseContext::inherit(node, inherited),
            ));
        }
    }
}
pub(super) struct Inventory<'a, 'input> {
    pub jobs: Vec<Job<'a, 'input>>,
    addressings: Vec<EffectiveAddressing<'a, 'input>>,
    timelines: Vec<Timeline<'a, 'input>>,
    lists: Vec<addressing::List<'a, 'input>>,
    terminals: Vec<Terminal>,
}
impl<'a, 'input> Inventory<'a, 'input> {
    pub fn build(
        root: Node<'a, 'input>,
        context: &LintContext<'_>,
        sink: &mut DiagnosticSink<'_>,
    ) -> Option<Self> {
        let mut inventory = Self {
            jobs: Vec::new(),
            addressings: Vec::new(),
            timelines: Vec::new(),
            lists: Vec::new(),
            terminals: Vec::new(),
        };
        let durations = period::durations(root);
        let mut registrations = Registrations::default();
        let mut scopes = Scopes::default();
        let mut ids = BTreeSet::new();
        let mut representations = 0;
        let mut duplicate = None;
        for node in root.descendants() {
            if node.pi().is_some_and(|pi| pi.target == "xml-stylesheet") {
                not_checked(
                    sink,
                    "DASH-XML-EXTERNAL",
                    NotCheckedReason::UnsupportedFeature,
                    span(node),
                );
            }
            if !node.is_element() {
                continue;
            }
            if !scopes.enter(node, context.options.limits.max_depth) {
                not_checked(
                    sink,
                    "DASH-XML-LIMIT",
                    NotCheckedReason::ResourceLimit,
                    span(node),
                );
                return None;
            }
            if node.tag_name().namespace() != Some(NS) {
                inventory.jobs.push(Job::Element(node));
                continue;
            }
            let mut selected = scopes
                .addressings
                .last()
                .and_then(|(_, selected)| *selected);
            let name = node.tag_name().name();
            scopes.base(node);
            if name == "Period" {
                scopes.periods.push((node.range().end, node.id().get()));
            }
            if matches!(name, "Period" | "AdaptationSet" | "Representation") {
                selected = inventory.add_addressing(node, selected);
                scopes.addressings.push((node.range().end, selected));
            }
            match name {
                "Representation" => {
                    representations += 1;
                    if let Some(location) = duplicate_identity(node, &mut ids) {
                        duplicate = Some(location);
                    }
                    let period = scopes.periods.last().map(|(_, id)| *id);
                    let duration = period
                        .and_then(|id| durations.get(&id))
                        .copied()
                        .unwrap_or(Err(NotCheckedReason::InvalidPrerequisite));
                    let (timeline_node, urls_node) = selected.map_or((None, None), |index| {
                        let selected = &inventory.addressings[index];
                        (selected.timeline, selected.urls)
                    });
                    let timeline =
                        timeline_node.map(|node| inventory.add_timeline(node, &mut registrations));
                    let list = urls_node.map(|node| inventory.add_list(node, &mut registrations));
                    inventory.jobs.push(Job::Representation {
                        node,
                        template: selected,
                        duration,
                        timeline,
                        list,
                        base: scopes
                            .bases
                            .last()
                            .map_or_else(addressing::BaseContext::default, |(_, context)| *context),
                    });
                    inventory.add_terminal(selected, period, duration, &mut registrations);
                }
                "SegmentTimeline" => {
                    inventory.add_timeline(node, &mut registrations);
                }
                "S" if node
                    .parent()
                    .is_some_and(|parent| parent.has_tag_name((NS, "SegmentTimeline"))) => {}
                _ => inventory.jobs.push(Job::Element(node)),
            }
        }
        check(
            sink,
            "DASH-PRESENTATION-REPRESENTATIONS",
            representations > 0,
            span(root),
        );
        identity_coverage(duplicate, root, sink);
        Some(inventory)
    }

    fn add_timeline(&mut self, node: Node<'a, 'input>, registrations: &mut Registrations) -> usize {
        *registrations
            .timelines
            .entry(node.id().get())
            .or_insert_with(|| {
                let index = self.timelines.len();
                self.timelines.push(Timeline {
                    node,
                    facts: OnceLock::new(),
                });
                self.jobs.push(Job::Timeline(index));
                index
            })
    }

    fn add_list(&mut self, node: Node<'a, 'input>, registrations: &mut Registrations) -> usize {
        *registrations
            .lists
            .entry(node.id().get())
            .or_insert_with(|| {
                let index = self.lists.len();
                self.lists.push(addressing::List::new(node));
                self.jobs.push(Job::List(index));
                index
            })
    }

    fn add_terminal(
        &mut self,
        selected: Option<usize>,
        period: Option<u32>,
        duration: terminal::PeriodDuration,
        registrations: &mut Registrations,
    ) {
        let Some(template) = selected.map(|index| &self.addressings[index]) else {
            return;
        };
        let Some(node) = template.timeline else {
            return;
        };
        let (timescale, offset) = (template.timescale, template.offset);
        let timeline = self.add_timeline(node, registrations);
        if registrations
            .contexts
            .insert((timeline, period, timescale, offset))
        {
            let index = self.terminals.len();
            self.terminals.push(Terminal {
                timeline,
                duration,
                timescale,
                offset,
            });
            self.jobs.push(Job::Terminal(index));
        }
    }

    fn timeline(&self, index: usize, sink: &mut DiagnosticSink<'_>) -> timeline::Facts {
        let timeline = &self.timelines[index];
        *timeline.facts.get_or_init(|| {
            attributes::check(timeline.node, sink);
            timeline::check(timeline.node, sink)
        })
    }

    fn add_addressing(
        &mut self,
        owner: Node<'a, 'input>,
        inherited: Option<usize>,
    ) -> Option<usize> {
        let Some(node) = owner.children().find(|child| {
            child.tag_name().namespace() == Some(NS)
                && matches!(
                    child.tag_name().name(),
                    "SegmentTemplate" | "SegmentList" | "SegmentBase"
                )
        }) else {
            return inherited;
        };
        let template =
            EffectiveAddressing::inherit(node, inherited.map(|index| &self.addressings[index]));
        let index = self.addressings.len();
        self.addressings.push(template);
        self.jobs.push(Job::Template(index));
        Some(index)
    }

    pub fn check(&self, job: Job<'a, 'input>, sink: &mut DiagnosticSink<'_>) {
        match job {
            Job::Element(node) => element(node, sink),
            Job::Template(index) => self.addressings[index].check(sink),
            Job::List(index) => self.lists[index].check(sink),
            Job::Timeline(index) => {
                self.timeline(index, sink);
            }
            Job::Terminal(index) => {
                let context = &self.terminals[index];
                if let Some(run) = self.timeline(context.timeline, sink).terminal {
                    terminal::check(
                        run,
                        context.duration,
                        context.timescale,
                        context.offset,
                        sink,
                    );
                }
            }
            Job::Representation {
                node,
                template,
                duration,
                timeline,
                list,
                base,
            } => {
                element(node, sink);
                let valid = node
                    .attribute("id")
                    .is_some_and(attributes::representation_identifier)
                    && node
                        .attribute("bandwidth")
                        .and_then(|value| unsigned(value, u64::from(u32::MAX)))
                        .is_some()
                    && node
                        .parent()
                        .is_some_and(|parent| parent.has_tag_name((NS, "AdaptationSet")));
                check(sink, "DASH-REPRESENTATION-ATTRIBUTES", valid, span(node));
                if template.is_none_or(|index| !self.addressings[index].complete()) {
                    not_checked(
                        sink,
                        "DASH-ADDRESSING-COVERAGE",
                        NotCheckedReason::UnsupportedFeature,
                        span(node),
                    );
                }
                if let Some(selected) = template {
                    addressing::representation(
                        &self.addressings[selected],
                        node,
                        duration,
                        timeline.map(|index| self.timeline(index, sink)),
                        list.map(|index| &self.lists[index]),
                        base,
                        sink,
                    );
                }
            }
        }
    }
}

fn element(node: Node<'_, '_>, sink: &mut DiagnosticSink<'_>) {
    if node.tag_name().namespace() == Some("http://www.w3.org/2001/XInclude")
        || node
            .attributes()
            .any(|attribute| attribute.namespace() == Some("http://www.w3.org/1999/xlink"))
    {
        not_checked(
            sink,
            "DASH-XML-EXTERNAL",
            NotCheckedReason::UnsupportedFeature,
            span(node),
        );
    }
    if node.tag_name().namespace() != Some(NS) {
        if protection::extension(node, sink) {
            return;
        }
        not_checked(
            sink,
            "DASH-FEATURE-COVERAGE",
            NotCheckedReason::UnsupportedFeature,
            span(node),
        );
        return;
    }
    attributes::check(node, sink);
    addressing::element(node, sink);
    let name = node.tag_name().name();
    if matches!(name, "Period" | "AdaptationSet" | "Representation") {
        let count = node
            .children()
            .filter(|child| {
                child.is_element()
                    && child.tag_name().namespace() == Some(NS)
                    && matches!(
                        child.tag_name().name(),
                        "SegmentBase" | "SegmentList" | "SegmentTemplate"
                    )
            })
            .count();
        check(sink, "DASH-SEGMENT-ADDRESSING", count <= 1, span(node));
    }
    match name {
        "ContentProtection" => protection::descriptor(node, sink),
        "MPD" => {
            if let Some(value) = node.attribute("mediaPresentationDuration") {
                duration_check(sink, "DASH-PERIOD-TIMING", Some(value), node);
            }
        }
        "Period" => {
            for name in ["start", "duration"] {
                if let Some(value) = node.attribute(name) {
                    duration_check(sink, "DASH-PERIOD-TIMING", Some(value), node);
                }
            }
        }
        _ => {}
    }
}

fn duplicate_identity<'a>(
    node: Node<'a, '_>,
    ids: &mut BTreeSet<(Option<u32>, &'a str)>,
) -> Option<crate::Span> {
    let period = node
        .ancestors()
        .find(|node| node.has_tag_name((NS, "Period")))
        .map(|node| node.id().get());
    node.attribute("id")
        .and_then(|id| (!ids.insert((period, id))).then(|| span(node)))
}
fn identity_coverage(
    duplicate: Option<crate::Span>,
    root: Node<'_, '_>,
    sink: &mut DiagnosticSink<'_>,
) {
    if let Some(location) = duplicate {
        not_checked(
            sink,
            "DASH-REPRESENTATION-IDENTITY",
            NotCheckedReason::InvalidPrerequisite,
            location,
        );
    } else {
        check(sink, "DASH-REPRESENTATION-IDENTITY", true, span(root));
    }
}
