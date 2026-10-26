use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    num::NonZeroUsize,
};

/// A caller-assigned opaque identifier; no source path is required.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct DocumentId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Format {
    Hls,
    Dash,
}

/// Borrowed input. Debug intentionally omits its bytes.
#[derive(Clone, Copy)]
pub struct Document<'a> {
    pub id: DocumentId,
    pub format: Format,
    pub bytes: &'a [u8],
    /// Optional absolute retrieval identity for resolving relative key identifiers.
    /// It is metadata only: the library never opens or requests this URI.
    /// At most 8192 bytes; larger metadata exhausts a resource limit before parsing.
    pub base_uri: Option<&'a str>,
}
/// Input metadata for reserving resources before retaining a presentation's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentSize {
    pub id: DocumentId,
    pub format: Format,
    pub byte_count: usize,
}
impl fmt::Debug for Document<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Document")
            .field("id", &self.id)
            .field("format", &self.format)
            .field("byte_count", &self.bytes.len())
            .finish()
    }
}

/// An exact reference binding supplied by the caller, never a fetch instruction.
/// The total reference bytes cannot exceed the supplied manifest bytes; bindings
/// must be extracted from those manifests rather than containing unrelated data.
#[derive(Clone, Copy)]
pub struct ManifestLink<'a> {
    pub from: DocumentId,
    pub reference: &'a str,
    pub to: DocumentId,
}
impl fmt::Debug for ManifestLink<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManifestLink")
            .field("from", &self.from)
            .field("to", &self.to)
            .finish_non_exhaustive()
    }
}

/// A playlist reference, excluding media, maps and keys. Debug omits the URI.
#[derive(Clone, Copy)]
pub struct ManifestReference<'a> {
    pub uri: &'a str,
    pub span: Span,
}
impl fmt::Debug for ManifestReference<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManifestReference")
            .field("span", &self.span)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferenceError {
    InvalidSyntax,
    ResourceLimit,
    UnsupportedFeature,
}
impl fmt::Display for ReferenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "manifest-reference-invalid:{self:?}")
    }
}
impl std::error::Error for ReferenceError {}

#[derive(Clone, Copy, Debug)]
pub struct Presentation<'a> {
    pub entrypoint: DocumentId,
    pub documents: &'a [Document<'a>],
    pub links: &'a [ManifestLink<'a>],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Profile {
    HlsBaseManifest,
    AppleVodManifest,
    DashStaticManifest,
}
impl Profile {
    pub fn format(self) -> Format {
        match self {
            Self::HlsBaseManifest | Self::AppleVodManifest => Format::Hls,
            Self::DashStaticManifest => Format::Dash,
        }
    }
    pub fn revision(self) -> &'static str {
        match self {
            Self::DashStaticManifest => "2026-10-26.1",
            Self::HlsBaseManifest | Self::AppleVodManifest => "2026-10-21.1",
        }
    }
}

/// Hard logical limits. The executor additionally admits parser work against its
/// documented conservative allocation model, not a process RSS guarantee.
#[derive(Clone, Debug)]
pub struct Limits {
    pub max_document_bytes: usize,
    pub max_total_bytes: usize,
    pub max_documents: usize,
    pub max_lines: usize,
    pub max_line_bytes: usize,
    pub max_xml_nodes: u32,
    pub max_xml_attributes_per_element: usize,
    /// Maximum distinct lexical namespace bindings (prefix and raw URI) per document.
    pub max_xml_namespace_bindings: usize,
    pub max_depth: usize,
    pub max_references: usize,
    pub max_diagnostics: usize,
    pub max_workers: usize,
    pub memory_budget_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_document_bytes: 4 * 1024 * 1024,
            max_total_bytes: 16 * 1024 * 1024,
            max_documents: 1024,
            max_lines: 131_072,
            max_line_bytes: 4 * 1024 * 1024,
            max_xml_nodes: 100_000,
            max_xml_attributes_per_element: 128,
            max_xml_namespace_bindings: 128,
            max_depth: 64,
            max_references: 16_384,
            max_diagnostics: 256,
            max_workers: 32,
            memory_budget_bytes: 1024 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub profile: Profile,
    pub limits: Limits,
    /// None uses available parallelism; an explicit count is an upper bound.
    pub workers: Option<NonZeroUsize>,
}
/// Conservative admission accounting, including retained reports and parser work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ExecutionEstimate {
    pub input_bytes: usize,
    pub retained_bytes: usize,
    pub parser_bytes_per_worker: usize,
    pub parser_concurrency: usize,
    pub semantic_bytes_per_worker: usize,
    pub requested_workers: usize,
    pub admitted_workers: usize,
    pub estimated_peak_bytes: usize,
}
impl Options {
    pub fn new(profile: Profile) -> Self {
        Self {
            profile,
            limits: Limits::default(),
            workers: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Location {
    pub document: DocumentId,
    pub span: Span,
    /// One-based line and Unicode scalar column in the original UTF-8 bytes.
    pub line: usize,
    pub column: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    Error,
    Warning,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuleScope {
    InScope,
    OutsideScope,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotCheckedReason {
    MissingLinkedManifest,
    UnsupportedFeature,
    RequiresMediaEvidence,
    RequiresDeliveryEvidence,
    ResourceLimit,
    InvalidPrerequisite,
    MissingReferenceBase,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "status", content = "reason")]
pub enum CheckStatus {
    Passed,
    Failed,
    NotChecked(NotCheckedReason),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct RuleReference {
    pub url: &'static str,
    pub section: &'static str,
    pub revision: &'static str,
}
pub(crate) const RESOURCE_REFERENCE: RuleReference = RuleReference {
    url: "https://github.com/kazuhide/drmr/tree/main/crates/drmr-manifest#resource-limits",
    section: "resource-limits",
    revision: "2026-10-21.1",
};
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rule {
    pub id: &'static str,
    pub severity: Severity,
    pub scope: RuleScope,
    pub message: &'static str,
    pub reference: RuleReference,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub rule_id: &'static str,
    pub severity: Severity,
    pub message_code: &'static str,
    pub location: Option<Location>,
    pub reference: RuleReference,
}
impl Diagnostic {
    pub(crate) fn order_key(&self) -> (usize, &'static str, Severity, &'static str) {
        (
            self.location.map_or(0, |location| location.span.start),
            self.rule_id,
            self.severity,
            self.message_code,
        )
    }
}
impl Ord for Diagnostic {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.order_key()
            .cmp(&other.order_key())
            .then_with(|| self.location.cmp(&other.location))
            .then_with(|| self.reference.cmp(&other.reference))
    }
}
impl PartialOrd for Diagnostic {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RuleCoverage {
    pub document: DocumentId,
    pub rule_id: &'static str,
    pub scope: RuleScope,
    pub severity: Severity,
    pub status: CheckStatus,
    pub passed_checks: usize,
    pub failed_checks: usize,
    pub not_checked_checks: usize,
    pub not_checked_reasons: BTreeSet<NotCheckedReason>,
    pub reference: RuleReference,
}
impl RuleCoverage {
    pub(crate) fn merge(&mut self, other: Self) {
        self.passed_checks += other.passed_checks;
        self.failed_checks += other.failed_checks;
        self.not_checked_checks += other.not_checked_checks;
        self.not_checked_reasons.extend(other.not_checked_reasons);
        self.scope = self.scope.min(other.scope);
        self.severity = self.severity.min(other.severity);
        self.refresh();
    }
    pub(crate) fn refresh(&mut self) {
        self.status = if self.failed_checks > 0 {
            CheckStatus::Failed
        } else if let Some(reason) = self.not_checked_reasons.first() {
            CheckStatus::NotChecked(*reason)
        } else {
            CheckStatus::Passed
        };
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub scope: &'static str,
    pub engine_revision: &'static str,
    pub profile: Profile,
    pub profile_revision: &'static str,
    pub errors: usize,
    pub warnings: usize,
    pub truncated: bool,
    pub incomplete: bool,
    pub document_count: usize,
    pub workers_used: usize,
    pub diagnostics: Vec<Diagnostic>,
    pub coverage: Vec<RuleCoverage>,
}
impl Report {
    /// A clean result only within the named manifest scope, never full conformance.
    pub fn is_clean_for_scope(&self) -> bool {
        self.errors == 0 && !self.truncated && !self.incomplete
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigurationError {
    InvalidLimits,
    EmptyPresentation,
    DuplicateDocumentId,
    InvalidEntrypoint,
    InvalidLink,
    FormatMismatch,
}
impl fmt::Display for ConfigurationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "manifest-lint-configuration-invalid:{self:?}")
    }
}
impl std::error::Error for ConfigurationError {}

/// Internal module context. Modules inspect only supplied input and bindings.
pub(crate) struct LintContext<'a> {
    pub options: &'a Options,
    pub semantic_workers: usize,
    documents: BTreeMap<DocumentId, Document<'a>>,
    links: BTreeMap<(DocumentId, &'a str), DocumentId>,
}
impl LintContext<'_> {
    pub fn new<'a>(
        options: &'a Options,
        presentation: Option<Presentation<'a>>,
    ) -> LintContext<'a> {
        LintContext {
            options,
            semantic_workers: 1,
            documents: presentation.map_or_else(BTreeMap::new, |graph| {
                graph.documents.iter().map(|doc| (doc.id, *doc)).collect()
            }),
            links: presentation.map_or_else(BTreeMap::new, |graph| {
                graph
                    .links
                    .iter()
                    .map(|link| ((link.from, link.reference), link.to))
                    .collect()
            }),
        }
    }
    pub fn linked(&self, from: DocumentId, reference: &str) -> Option<Document<'_>> {
        self.documents
            .get(self.links.get(&(from, reference))?)
            .copied()
    }
    pub fn outgoing(&self, from: DocumentId) -> impl Iterator<Item = (&str, DocumentId)> {
        self.links
            .range((from, "")..)
            .take_while(move |((id, _), _)| *id == from)
            .map(|((_, reference), to)| (*reference, *to))
    }
}
