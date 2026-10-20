use crate::{
    ConfigurationError, Diagnostic, Document, DocumentSize, ExecutionEstimate, Format, Options,
    RuleCoverage, uri::MAX_BASE_URI_BYTES,
};
use std::{mem::size_of, thread};
const RULE_RESERVE: usize = 256;
const MIN_PARALLEL_BYTES: usize = 64 * 1024;
const ABSOLUTE_MAX_WORKERS: usize = 64;

pub(super) fn configured(options: &Options) -> Result<(), ConfigurationError> {
    let limits = &options.limits;
    if [
        limits.max_document_bytes,
        limits.max_total_bytes,
        limits.max_documents,
        limits.max_lines,
        limits.max_line_bytes,
        limits.max_xml_attributes_per_element,
        limits.max_xml_namespace_bindings,
        limits.max_depth,
        limits.max_references,
        limits.max_diagnostics,
        limits.max_workers,
        limits.memory_budget_bytes,
    ]
    .contains(&0)
        || limits.max_xml_nodes == 0
    {
        return Err(ConfigurationError::InvalidLimits);
    }
    Ok(())
}

/// Estimate this call's admitted parser concurrency without parsing any document.
///
/// Input bytes, an equal bound for caller link metadata, 8 KiB of caller base URI
/// metadata per document, all bounded coverage records, graph indexes and the final
/// diagnostic sink are reserved first. Each parser task reserves 64 bytes per byte of the
/// largest document plus 64 KiB. Each semantic worker reserves two bounded
/// diagnostic sinks, rule coverage and 64 KiB overhead. HLS graph checks reuse
/// retained child facts without reparsing documents. The estimate
/// bounds configured work; it is deliberately conservative, not an RSS measurement.
pub fn estimate_resources(
    documents: &[Document<'_>],
    options: &Options,
) -> Result<ExecutionEstimate, ConfigurationError> {
    estimate(
        documents.len(),
        documents.iter().map(|doc| (doc.format, doc.bytes.len())),
        options,
    )
}

/// Apply exactly the same admission formula using metadata only, before input bytes
/// are loaded. Callers must verify that subsequently loaded bytes match this inventory.
pub fn estimate_document_sizes(
    documents: &[DocumentSize],
    options: &Options,
) -> Result<ExecutionEstimate, ConfigurationError> {
    estimate(
        documents.len(),
        documents.iter().map(|doc| (doc.format, doc.byte_count)),
        options,
    )
}

fn estimate(
    count: usize,
    sizes: impl Iterator<Item = (Format, usize)>,
    options: &Options,
) -> Result<ExecutionEstimate, ConfigurationError> {
    configured(options)?;
    if count == 0 {
        return Err(ConfigurationError::EmptyPresentation);
    }
    if count > options.limits.max_documents {
        return Ok(refused());
    }
    let mut input_bytes = Some(0_usize);
    let mut largest = 0;
    let mut work_units = 0_usize;
    for (format, size) in sizes {
        if size > options.limits.max_document_bytes {
            return Ok(refused());
        }
        input_bytes = input_bytes.and_then(|sum| sum.checked_add(size));
        if input_bytes.is_none_or(|sum| sum > options.limits.max_total_bytes) {
            return Ok(refused());
        }
        largest = largest.max(size);
        // Metadata reserves an upper bound for shared-DOM semantic jobs before
        // parsing reveals their exact count. Multiple-document calls never nest pools.
        let units = if count == 1 && format == Format::Dash && size >= MIN_PARALLEL_BYTES {
            (size / 1024).max(1)
        } else {
            1
        };
        work_units = work_units.saturating_add(units);
    }
    let estimate = || -> Option<ExecutionEstimate> {
        let input_bytes = input_bytes?;
        let diagnostic_bytes = options
            .limits
            .max_diagnostics
            .checked_mul(size_of::<Diagnostic>().checked_mul(4)?)?;
        let coverage_bytes = count
            .checked_mul(RULE_RESERVE)?
            .checked_mul(size_of::<RuleCoverage>().checked_add(128)?)?;
        let graph_bytes = options
            .limits
            .max_references
            .checked_mul(256)?
            .checked_add(count.checked_mul(MAX_BASE_URI_BYTES.checked_add(256)?)?)?;
        let retained_bytes = input_bytes
            .checked_mul(18)?
            .checked_add(diagnostic_bytes)?
            .checked_add(coverage_bytes)?
            .checked_add(graph_bytes)?;
        let parser_bytes_per_worker = largest.checked_mul(64)?.checked_add(64 * 1024)?;
        let semantic_bytes_per_worker = diagnostic_bytes
            .checked_mul(2)?
            .checked_add(RULE_RESERVE.checked_mul(size_of::<RuleCoverage>().checked_add(128)?)?)?
            .checked_add(64 * 1024)?;
        let requested_workers = options
            .workers
            .map_or_else(
                || thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get),
                std::num::NonZeroUsize::get,
            )
            .min(options.limits.max_workers)
            .min(work_units)
            .min(ABSOLUTE_MAX_WORKERS);
        // A tiny master plus one dominant child has one substantial parser job.
        // Keep that measured skew inline; a single DASH MPD still admits its
        // independent shared-DOM semantic jobs.
        let dominant_document = count > 1 && largest >= input_bytes - input_bytes / 20;
        let ceiling = if input_bytes < MIN_PARALLEL_BYTES || dominant_document {
            requested_workers.min(1)
        } else {
            requested_workers
        };
        let mut admitted_workers = 0;
        let mut estimated_peak_bytes = retained_bytes;
        for workers in (1..=ceiling).rev() {
            let peak = retained_bytes
                .checked_add(workers.min(count).checked_mul(parser_bytes_per_worker)?)?
                .checked_add(workers.checked_mul(semantic_bytes_per_worker)?)?;
            if peak <= options.limits.memory_budget_bytes {
                admitted_workers = workers;
                estimated_peak_bytes = peak;
                break;
            }
        }
        Some(ExecutionEstimate {
            input_bytes,
            retained_bytes,
            parser_bytes_per_worker,
            parser_concurrency: admitted_workers.min(count),
            semantic_bytes_per_worker,
            requested_workers,
            admitted_workers,
            estimated_peak_bytes,
        })
    };
    Ok(estimate().unwrap_or_else(refused))
}

fn refused() -> ExecutionEstimate {
    ExecutionEstimate {
        input_bytes: usize::MAX,
        retained_bytes: usize::MAX,
        parser_bytes_per_worker: usize::MAX,
        parser_concurrency: 0,
        semantic_bytes_per_worker: usize::MAX,
        requested_workers: 1,
        admitted_workers: 0,
        estimated_peak_bytes: usize::MAX,
    }
}
