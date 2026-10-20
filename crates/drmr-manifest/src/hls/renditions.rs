use super::{
    attribute_types, check, rule,
    syntax::{self, Attribute, Line},
    unknown_attributes,
};
use crate::{CheckStatus, DiagnosticSink, LintContext, NotCheckedReason, Profile, Severity, Span};
use std::collections::{BTreeMap, BTreeSet};

type AutomaticSelection<'a> = (
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
);

struct Rendition<'a> {
    name: &'a str,
    signature: Vec<(&'a str, &'a str)>,
}
#[derive(Default)]
struct Group<'a> {
    members: BTreeMap<&'a str, Rendition<'a>>,
    defaults: usize,
    automatic: BTreeSet<AutomaticSelection<'a>>,
}

#[derive(Default)]
pub(super) struct Groups<'a>(BTreeMap<(&'a str, &'a str), Group<'a>>);

impl<'a> Groups<'a> {
    pub fn contains(&self, key: &(&str, &str)) -> bool {
        self.0.contains_key(key)
    }
    pub fn declare(
        &mut self,
        attributes: &[Attribute<'a>],
        line: Line<'_>,
        context: &LintContext<'_>,
        sink: &mut DiagnosticSink<'_>,
        minimum: &mut u64,
    ) {
        rendition(attributes, line, context, sink, minimum, &mut self.0);
    }
    pub fn finish(&self, span: Span, sink: &mut DiagnosticSink<'_>) {
        let mut representatives = BTreeMap::<&str, &Group<'_>>::new();
        for ((kind, _), group) in &self.0 {
            if let Some(previous) = representatives.get(kind) {
                let same = previous.members.len() == group.members.len()
                    && group.members.iter().all(|(name, member)| {
                        previous.members.get(name).is_some_and(|other| {
                            other.signature == member.signature && other.name == member.name
                        })
                    });
                check(
                    sink,
                    rule(
                        "HLS-RENDITION-GROUPS",
                        "hls-rendition-groups-inconsistent",
                        "4.3.4.1.1",
                    ),
                    same,
                    span,
                );
            } else {
                representatives.insert(*kind, group);
            }
        }
    }
}
fn rendition<'a>(
    attributes: &[Attribute<'a>],
    line: Line<'_>,
    context: &LintContext<'_>,
    sink: &mut DiagnosticSink<'_>,
    minimum: &mut u64,
    groups: &mut BTreeMap<(&'a str, &'a str), Group<'a>>,
) {
    schema(attributes, sink);
    let value = |name| syntax::get(attributes, name).map(|attribute| attribute.value);
    let kind = value("TYPE");
    let group = value("GROUP-ID");
    let name = value("NAME");
    let default = value("DEFAULT") == Some("YES");
    let valid = matches!(
        kind,
        Some("AUDIO" | "VIDEO" | "SUBTITLES" | "CLOSED-CAPTIONS")
    ) && group.is_some()
        && name.is_some()
        && ["DEFAULT", "AUTOSELECT", "FORCED"]
            .iter()
            .all(|name| value(name).is_none_or(|value| matches!(value, "YES" | "NO")))
        && (!default || value("AUTOSELECT") != Some("NO"))
        && (kind == Some("SUBTITLES") || value("FORCED").is_none())
        && (kind != Some("SUBTITLES") || value("URI").is_some())
        && (kind != Some("CLOSED-CAPTIONS")
            || value("URI").is_none() && value("INSTREAM-ID").is_some_and(instream))
        && (kind == Some("CLOSED-CAPTIONS") || value("INSTREAM-ID").is_none());
    check(
        sink,
        rule(
            "HLS-RENDITION",
            "hls-rendition-declaration-invalid",
            "4.3.4.1",
        ),
        valid,
        line.span,
    );
    if value("INSTREAM-ID").is_some_and(|value| value.starts_with("SERVICE")) {
        *minimum = (*minimum).max(7);
    }
    channels(attributes, sink);
    if context.options.profile == Profile::AppleVodManifest {
        super::apple::rendition(attributes, line, sink);
    }
    register(attributes, line, groups, sink);
}

fn instream(value: &str) -> bool {
    matches!(value, "CC1" | "CC2" | "CC3" | "CC4")
        || value
            .strip_prefix("SERVICE")
            .filter(|digits| !digits.starts_with('0'))
            .and_then(syntax::integer)
            .is_some_and(|number| (1..=63).contains(&number))
}

fn schema(attributes: &[Attribute<'_>], sink: &mut DiagnosticSink<'_>) {
    unknown_attributes(
        attributes,
        &[
            "TYPE",
            "URI",
            "GROUP-ID",
            "LANGUAGE",
            "ASSOC-LANGUAGE",
            "NAME",
            "DEFAULT",
            "AUTOSELECT",
            "FORCED",
            "INSTREAM-ID",
            "CHARACTERISTICS",
            "CHANNELS",
        ],
        sink,
    );
    attribute_types(
        attributes,
        &[
            "URI",
            "GROUP-ID",
            "LANGUAGE",
            "ASSOC-LANGUAGE",
            "NAME",
            "INSTREAM-ID",
            "CHARACTERISTICS",
            "CHANNELS",
        ],
        &["TYPE", "DEFAULT", "AUTOSELECT", "FORCED"],
        sink,
    );
}
fn channels(attributes: &[Attribute<'_>], sink: &mut DiagnosticSink<'_>) {
    if let Some(channels) = syntax::get(attributes, "CHANNELS") {
        check(
            sink,
            rule("HLS-CHANNELS", "hls-audio-channel-count-invalid", "4.3.4.1"),
            channels
                .value
                .split('/')
                .next()
                .and_then(syntax::integer)
                .is_some_and(|count| count > 0),
            channels.span,
        );
        if channels.value.contains('/') {
            sink.check(
                rule(
                    "HLS-CHANNEL-EXTENSION",
                    "hls-channel-extension-not-checked",
                    "4.3.4.1",
                ),
                CheckStatus::NotChecked(NotCheckedReason::UnsupportedFeature),
                Some(channels.span),
            );
        }
    }
}
fn register<'a>(
    attributes: &[Attribute<'a>],
    line: Line<'_>,
    groups: &mut BTreeMap<(&'a str, &'a str), Group<'a>>,
    sink: &mut DiagnosticSink<'_>,
) {
    let value = |name| syntax::get(attributes, name).map(|attribute| attribute.value);
    let kind = value("TYPE");
    let group = value("GROUP-ID");
    let name = value("NAME");
    let default = value("DEFAULT") == Some("YES");
    if let (Some(kind), Some(group), Some(name)) = (kind, group, name) {
        let group = groups.entry((kind, group)).or_default();
        group.defaults += usize::from(default);
        check(
            sink,
            rule(
                "HLS-RENDITION-DEFAULT",
                "hls-rendition-default-duplicated",
                "4.3.4.1.1",
            ),
            group.defaults <= 1,
            line.span,
        );
        let mut signature = attributes
            .iter()
            .filter(|attribute| !matches!(attribute.name, "GROUP-ID" | "URI" | "CHANNELS"))
            .map(|attribute| (attribute.name, attribute.value))
            .collect::<Vec<_>>();
        signature.sort_unstable();
        let duplicate = group
            .members
            .insert(name, Rendition { name, signature })
            .is_some();
        check(
            sink,
            rule(
                "HLS-RENDITION-NAMES",
                "hls-rendition-name-duplicated",
                "4.3.4.1.1",
            ),
            !duplicate,
            line.span,
        );
        if value("AUTOSELECT") == Some("YES") {
            let mut requirement = rule(
                "HLS-RENDITION-AUTOSELECT",
                "hls-automatic-renditions-indistinguishable",
                "4.3.4.1.1",
            );
            requirement.severity = Severity::Warning;
            check(
                sink,
                requirement,
                group.automatic.insert((
                    value("LANGUAGE"),
                    value("ASSOC-LANGUAGE"),
                    value("FORCED"),
                    value("CHARACTERISTICS"),
                )),
                line.span,
            );
        }
    }
}
