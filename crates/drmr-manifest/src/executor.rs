mod admission;
use crate::{
    CheckStatus, ConfigurationError, DiagnosticSink, Document, DocumentId, Format, LintContext,
    NotCheckedReason, Options, Presentation, Report, Rule, RuleCoverage, RuleScope, Severity, dash,
    hls, model::RESOURCE_REFERENCE, uri::MAX_BASE_URI_BYTES,
};
use admission::configured;
pub use admission::{estimate_document_sizes, estimate_resources};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

const RESOURCE: Rule = Rule {
    id: "MANIFEST-RESOURCE-LIMIT",
    severity: Severity::Error,
    scope: RuleScope::InScope,
    message: "manifest-resource-limit",
    reference: RESOURCE_REFERENCE,
};
const GRAPH: Rule = Rule {
    id: "MANIFEST-GRAPH-VALID",
    severity: Severity::Error,
    scope: RuleScope::InScope,
    message: "manifest-graph-invalid",
    reference: RESOURCE.reference,
};

/// Lint one supplied document. Child-manifest checks explicitly remain unresolved.
pub fn lint_document(
    document: Document<'_>,
    options: &Options,
) -> Result<Report, ConfigurationError> {
    run(&[document], None, options)
}

/// Lint a fully supplied presentation graph without resolving or fetching references.
pub fn lint_presentation(
    presentation: Presentation<'_>,
    options: &Options,
) -> Result<Report, ConfigurationError> {
    run(presentation.documents, Some(presentation), options)
}

fn run(
    documents: &[Document<'_>],
    presentation: Option<Presentation<'_>>,
    options: &Options,
) -> Result<Report, ConfigurationError> {
    configured(options)?;
    if documents.is_empty() {
        return Err(ConfigurationError::EmptyPresentation);
    }
    // Refuse counts before scanning caller metadata, and byte/base limits before
    // indexing references or parsing any document.
    if documents.len() > options.limits.max_documents
        || presentation.is_some_and(|graph| graph.links.len() > options.limits.max_references)
        || documents.iter().any(|document| {
            document.bytes.len() > options.limits.max_document_bytes
                || document
                    .base_uri
                    .is_some_and(|base| base.len() > MAX_BASE_URI_BYTES)
        })
    {
        return Ok(resource_limited(documents, options));
    }
    let estimate = estimate_resources(documents, options)?;
    if estimate.input_bytes > options.limits.max_total_bytes
        || presentation.is_some_and(|graph| {
            graph
                .links
                .iter()
                .try_fold(0_usize, |total, link| {
                    total
                        .checked_add(link.reference.len())
                        .filter(|total| *total <= estimate.input_bytes)
                })
                .is_none()
        })
        || estimate.admitted_workers == 0
    {
        return Ok(resource_limited(documents, options));
    }
    let mut report = new_report(documents.len(), options);
    validate_presentation(documents, presentation, options)?;
    let order: BTreeMap<DocumentId, usize> = documents
        .iter()
        .enumerate()
        .map(|(index, doc)| (doc.id, index))
        .collect();
    let mut context = LintContext::new(options, presentation);
    if documents.len() == 1 && documents[0].format == Format::Dash {
        context.semantic_workers = estimate.admitted_workers;
    }
    let mut facts = BTreeMap::new();
    if let Some(graph) = presentation {
        let mut sink = DiagnosticSink::new(documents[0], options.limits.max_diagnostics);
        graph_check(graph, options, &mut sink);
        append(&mut report, sink, options.limits.max_diagnostics, &order);
    }
    if estimate.admitted_workers == 1 || documents.len() == 1 {
        for document in documents {
            let result = lint_one(*document, &context);
            if let Some(summary) = result.facts {
                facts.insert(document.id, summary);
            }
            append(
                &mut report,
                result.sink,
                options.limits.max_diagnostics,
                &order,
            );
        }
    } else {
        collect_parallel(
            documents,
            &context,
            estimate.admitted_workers,
            &mut report,
            &order,
            &mut facts,
        );
    }
    if presentation.is_some() {
        let graph_facts = hls::GraphFacts::new(&facts);
        for document in documents {
            if let Some(summary) = facts.get(&document.id) {
                let mut sink = DiagnosticSink::new(*document, options.limits.max_diagnostics);
                hls::lint_graph(document, summary, &facts, &graph_facts, &context, &mut sink);
                append(&mut report, sink, options.limits.max_diagnostics, &order);
            }
        }
    }
    report.diagnostics.sort_by_key(|diagnostic| {
        (
            diagnostic.location.map_or(0, |loc| order[&loc.document]),
            diagnostic.location.map_or(0, |loc| loc.span.start),
            diagnostic.rule_id,
            diagnostic.message_code,
        )
    });
    let mut coverage = BTreeMap::new();
    for item in std::mem::take(&mut report.coverage) {
        let key = (order[&item.document], item.rule_id);
        coverage
            .entry(key)
            .and_modify(|current: &mut RuleCoverage| current.merge(item.clone()))
            .or_insert(item);
    }
    report.coverage = coverage.into_values().collect();
    Ok(report)
}

fn resource_limited(documents: &[Document<'_>], options: &Options) -> Report {
    let mut report = new_report(documents.len(), options);
    let mut sink = DiagnosticSink::new(documents[0], options.limits.max_diagnostics);
    sink.check(
        RESOURCE,
        CheckStatus::NotChecked(NotCheckedReason::ResourceLimit),
        None,
    );
    append(
        &mut report,
        sink,
        options.limits.max_diagnostics,
        &BTreeMap::new(),
    );
    report
}

struct Linted<'a> {
    sink: DiagnosticSink<'a>,
    facts: Option<hls::HlsFacts>,
}

fn lint_one<'a>(document: Document<'a>, context: &LintContext<'_>) -> Linted<'a> {
    let mut sink = DiagnosticSink::new(document, context.options.limits.max_diagnostics);
    let mut lines = 0_usize;
    let over_lines = crate::lines::physical_lines(document.bytes).any(|line| {
        lines += 1;
        lines > context.options.limits.max_lines
            || line.len() > context.options.limits.max_line_bytes
    });
    if over_lines {
        sink.check(
            RESOURCE,
            CheckStatus::NotChecked(NotCheckedReason::ResourceLimit),
            None,
        );
        return Linted { sink, facts: None };
    }
    let facts = match document.format {
        Format::Hls => Some(hls::lint(&document, context, &mut sink)),
        Format::Dash => {
            dash::lint(&document, context, &mut sink);
            None
        }
    };
    Linted { sink, facts }
}

fn append(
    report: &mut Report,
    sink: DiagnosticSink<'_>,
    limit: usize,
    order: &BTreeMap<DocumentId, usize>,
) {
    report.errors += sink.errors;
    report.warnings += sink.warnings;
    report.truncated |= sink.truncated;
    report.incomplete |= sink.incomplete;
    report.workers_used = report.workers_used.max(sink.workers_used);
    report.diagnostics.extend(sink.diagnostics);
    report.coverage.extend(sink.coverage.into_values());
    // At most two bounded sinks coexist during aggregation. Final input ordering
    // is applied after collection, so scheduling cannot change retained diagnostics.
    report.diagnostics.sort_by_key(|diagnostic| {
        (
            diagnostic.location.map_or(0, |loc| order[&loc.document]),
            diagnostic.location.map_or(0, |loc| loc.span.start),
            diagnostic.rule_id,
            diagnostic.message_code,
        )
    });
    if report.diagnostics.len() > limit {
        report.diagnostics.truncate(limit);
        report.truncated = true;
        report.incomplete = true;
    }
}

fn graph_check(graph: Presentation<'_>, options: &Options, sink: &mut DiagnosticSink<'_>) {
    let indexes: BTreeMap<_, _> = graph
        .documents
        .iter()
        .enumerate()
        .map(|(index, document)| (document.id, index))
        .collect();
    let mut children = vec![Vec::new(); graph.documents.len()];
    let mut incoming = vec![0_usize; graph.documents.len()];
    for link in graph.links {
        let child = indexes[&link.to];
        children[indexes[&link.from]].push(child);
        incoming[child] += 1;
    }
    let mut ready: std::collections::VecDeque<_> = incoming
        .iter()
        .enumerate()
        .filter_map(|(index, count)| (*count == 0).then_some(index))
        .collect();
    let mut depths: Vec<Option<usize>> = vec![None; graph.documents.len()];
    depths[indexes[&graph.entrypoint]] = Some(0);
    let mut visited = 0;
    while let Some(index) = ready.pop_front() {
        visited += 1;
        for child in &children[index] {
            if let Some(depth) = depths[index] {
                depths[*child] = Some(depths[*child].unwrap_or(0).max(depth + 1));
            }
            incoming[*child] -= 1;
            if incoming[*child] == 0 {
                ready.push_back(*child);
            }
        }
    }
    check_graph(sink, visited == graph.documents.len());
    if depths
        .iter()
        .flatten()
        .any(|depth| *depth > options.limits.max_depth)
    {
        sink.check(
            GRAPH,
            CheckStatus::NotChecked(NotCheckedReason::ResourceLimit),
            None,
        );
    }
    if depths.iter().any(Option::is_none) {
        sink.check(
            GRAPH,
            CheckStatus::NotChecked(NotCheckedReason::InvalidPrerequisite),
            None,
        );
    }
}

fn check_graph(sink: &mut DiagnosticSink<'_>, valid: bool) {
    sink.check(
        GRAPH,
        if valid {
            CheckStatus::Passed
        } else {
            CheckStatus::Failed
        },
        None,
    );
}

fn validate_presentation(
    documents: &[Document<'_>],
    presentation: Option<Presentation<'_>>,
    options: &Options,
) -> Result<(), ConfigurationError> {
    let mut ids = BTreeSet::new();
    for document in documents {
        if !ids.insert(document.id) {
            return Err(ConfigurationError::DuplicateDocumentId);
        }
        if document.format != options.profile.format() {
            return Err(ConfigurationError::FormatMismatch);
        }
    }
    if let Some(graph) = presentation {
        if !ids.contains(&graph.entrypoint) {
            return Err(ConfigurationError::InvalidEntrypoint);
        }
        let mut bindings = BTreeSet::new();
        for link in graph.links {
            if link.reference.is_empty()
                || !ids.contains(&link.from)
                || !ids.contains(&link.to)
                || !bindings.insert((link.from, link.reference))
            {
                return Err(ConfigurationError::InvalidLink);
            }
        }
        if options.profile.format() == Format::Dash && !graph.links.is_empty() {
            return Err(ConfigurationError::InvalidLink);
        }
    }
    Ok(())
}

fn collect_parallel(
    documents: &[Document<'_>],
    context: &LintContext<'_>,
    workers: usize,
    report: &mut Report,
    order: &BTreeMap<DocumentId, usize>,
    facts: &mut BTreeMap<DocumentId, hls::HlsFacts>,
) {
    let options = context.options;
    report.workers_used = workers;
    let next = AtomicUsize::new(0);
    let (send, receive) = mpsc::sync_channel(workers);
    thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..workers {
            let send = send.clone();
            let next = &next;
            match thread::Builder::new().spawn_scoped(scope, move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(document) = documents.get(index) else {
                        break;
                    };
                    let mut result = lint_one(*document, context);
                    // Position indexes are parser working state, not retained results.
                    result.sink.release_positions();
                    if send.send((document.id, result)).is_err() {
                        break;
                    }
                }
            }) {
                Ok(handle) => handles.push(handle),
                Err(_) => report.incomplete = true,
            }
        }
        drop(send);
        let mut completed = 0;
        for (id, result) in receive {
            if let Some(summary) = result.facts {
                facts.insert(id, summary);
            }
            append(report, result.sink, options.limits.max_diagnostics, order);
            completed += 1;
        }
        for handle in handles {
            if handle.join().is_err() {
                report.incomplete = true;
            }
        }
        if completed != documents.len() {
            report.incomplete = true;
        }
    });
}

fn new_report(document_count: usize, options: &Options) -> Report {
    Report {
        schema_version: 1,
        scope: "manifest-only",
        engine_revision: env!("CARGO_PKG_VERSION"),
        profile: options.profile,
        profile_revision: options.profile.revision(),
        errors: 0,
        warnings: 0,
        truncated: false,
        incomplete: false,
        document_count,
        workers_used: 0,
        diagnostics: Vec::new(),
        coverage: Vec::new(),
    }
}
