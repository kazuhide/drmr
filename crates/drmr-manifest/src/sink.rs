use crate::{
    CheckStatus, Diagnostic, Document, Location, Rule, RuleCoverage, RuleScope, Severity, Span,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, OnceLock},
};

/// Bounded sink. Input values are never included in diagnostic text.
struct Positions {
    starts: Vec<usize>,
    scalar_checkpoints: Vec<usize>,
}
impl Positions {
    fn new(bytes: &[u8]) -> Self {
        let mut starts = vec![0];
        let mut scalar_checkpoints = Vec::with_capacity(bytes.len() / 64 + 1);
        let mut scalars = 0;
        for (index, byte) in bytes.iter().enumerate() {
            if index % 64 == 0 {
                scalar_checkpoints.push(scalars);
            }
            if byte & 0xc0 != 0x80 {
                scalars += 1;
            }
            let ending_length = crate::lines::ending_length(bytes, index);
            if ending_length > 0 {
                starts.push(index + ending_length);
            }
        }
        scalar_checkpoints.push(scalars);
        Self {
            starts,
            scalar_checkpoints,
        }
    }
    fn scalar_count(&self, bytes: &[u8], end: usize) -> usize {
        let checkpoint = end / 64;
        self.scalar_checkpoints[checkpoint]
            + bytes[checkpoint * 64..end]
                .iter()
                .filter(|byte| **byte & 0xc0 != 0x80)
                .count()
    }
}
pub(crate) struct DiagnosticSink<'a> {
    document: Document<'a>,
    limit: usize,
    line_starts: Arc<OnceLock<Positions>>,
    pub diagnostics: BTreeSet<Diagnostic>,
    pub coverage: BTreeMap<&'static str, RuleCoverage>,
    pub errors: usize,
    pub warnings: usize,
    pub truncated: bool,
    pub incomplete: bool,
    pub workers_used: usize,
}
impl<'a> DiagnosticSink<'a> {
    pub fn new(document: Document<'a>, limit: usize) -> Self {
        Self {
            document,
            limit,
            line_starts: Arc::new(OnceLock::new()),
            diagnostics: BTreeSet::new(),
            coverage: BTreeMap::new(),
            errors: 0,
            warnings: 0,
            truncated: false,
            incomplete: false,
            workers_used: 1,
        }
    }
    pub fn check(&mut self, rule: Rule, status: CheckStatus, span: Option<Span>) {
        let coverage = self
            .coverage
            .entry(rule.id)
            .or_insert_with(|| RuleCoverage {
                document: self.document.id,
                rule_id: rule.id,
                scope: rule.scope,
                severity: rule.severity,
                status: CheckStatus::Passed,
                passed_checks: 0,
                failed_checks: 0,
                not_checked_checks: 0,
                not_checked_reasons: BTreeSet::new(),
                reference: rule.reference,
            });
        match status {
            CheckStatus::Passed => coverage.passed_checks += 1,
            CheckStatus::Failed => {
                coverage.failed_checks += 1;
                coverage.status = status;
            }
            CheckStatus::NotChecked(reason) => {
                coverage.not_checked_checks += 1;
                coverage.not_checked_reasons.insert(reason);
                if coverage.failed_checks == 0 {
                    coverage.status = status;
                }
                if rule.scope == RuleScope::InScope {
                    self.incomplete = true;
                }
            }
        }
        coverage.refresh();
        if status != CheckStatus::Failed {
            return;
        }
        match rule.severity {
            Severity::Error => self.errors += 1,
            Severity::Warning => self.warnings += 1,
        }
        // Decide admission before computing positions. A stream of failures after
        // the retained prefix costs constant/logarithmic work per check.
        let key = (
            span.map_or(0, |span| span.start.min(self.document.bytes.len())),
            rule.id,
            rule.severity,
            rule.message,
        );
        if self.diagnostics.len() == self.limit
            && self
                .diagnostics
                .last()
                .is_none_or(|last| key > last.order_key())
        {
            self.truncated = true;
            self.incomplete = true;
            return;
        }
        let location = span.map(|span| self.location(span));
        let diagnostic = Diagnostic {
            rule_id: rule.id,
            severity: rule.severity,
            message_code: rule.message,
            location,
            reference: rule.reference,
        };
        self.diagnostics.insert(diagnostic);
        if self.diagnostics.len() > self.limit {
            self.diagnostics.pop_last();
            self.truncated = true;
            self.incomplete = true;
        }
    }
    pub fn release_positions(&mut self) {
        self.line_starts = Arc::new(OnceLock::new());
    }
    pub fn fork(&self) -> Self {
        let mut child = Self::new(self.document, self.limit);
        child.line_starts = Arc::clone(&self.line_starts);
        child
    }
    pub fn merge(&mut self, other: Self) {
        self.errors += other.errors;
        self.warnings += other.warnings;
        self.truncated |= other.truncated;
        self.incomplete |= other.incomplete;
        self.workers_used = self.workers_used.max(other.workers_used);
        for (id, coverage) in other.coverage {
            self.coverage
                .entry(id)
                .and_modify(|current| current.merge(coverage.clone()))
                .or_insert(coverage);
        }
        self.diagnostics.extend(other.diagnostics);
        while self.diagnostics.len() > self.limit {
            self.diagnostics.pop_last();
            self.truncated = true;
            self.incomplete = true;
        }
    }
    fn location(&mut self, mut span: Span) -> Location {
        span.start = span.start.min(self.document.bytes.len());
        span.end = span.end.max(span.start).min(self.document.bytes.len());
        let positions = self
            .line_starts
            .get_or_init(|| Positions::new(self.document.bytes));
        let line = positions
            .starts
            .partition_point(|start| *start <= span.start);
        let start = positions.starts[line - 1];
        let column = positions.scalar_count(self.document.bytes, span.start)
            - positions.scalar_count(self.document.bytes, start)
            + 1;
        Location {
            document: self.document.id,
            span,
            line,
            column,
        }
    }
}
