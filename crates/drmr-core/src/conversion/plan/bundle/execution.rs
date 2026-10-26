//! Output coordinators retain bounded metadata without holding their children's CPU tokens.
use super::{BundlePlan, ConversionError, Converter, OutputFormat, frozen};
use crate::resources::{
    Budget, ContextPermit, Hardware, ReservationError, TRACK_RESIDENCY_BYTES, Workload,
};

const NATIVE_LEAF_BYTES: u64 = 208 * 1024 * 1024;

struct Contexts {
    shared: u64,
    worker: u64,
    leaf: u64,
}

pub(super) struct Execution<'a> {
    converter: &'a Converter,
    #[cfg(test)]
    hardware: Option<Hardware>,
}

pub(super) struct Prepared {
    pub command: Converter,
    budget: Budget,
    workload: Workload,
    #[cfg(test)]
    hardware: Option<Hardware>,
}

impl Prepared {
    pub fn run<T: Send>(
        &self,
        plan: &BundlePlan,
        unit: impl Fn(usize) -> Result<T, ConversionError> + Sync,
    ) -> Result<Vec<T>, ConversionError> {
        let scheduler = super::super::super::execution::Scheduler::new(
            &self.command.backend,
            Some(self.budget.workers),
        );
        #[cfg(test)]
        let scheduler = self.hardware.map_or(scheduler, |hardware| {
            super::super::super::execution::Scheduler::observed(
                &self.command.backend,
                Some(self.budget.workers),
                hardware,
            )
        });
        scheduler.run(plan.outputs.len(), self.workload.tracks, 0, unit)
    }
}

impl<'a> Execution<'a> {
    pub fn new(converter: &'a Converter) -> Self {
        Self {
            converter,
            #[cfg(test)]
            hardware: None,
        }
    }

    pub fn prepare<T>(
        &self,
        plan: &BundlePlan,
        manifests: Option<&super::outputs::ManifestDemands>,
    ) -> Result<(Prepared, ContextPermit), ConversionError> {
        let command = self.converter;
        command.backend.check_cancelled()?;
        let workload = command
            .backend
            .runner
            .registered_workload()
            .unwrap_or_default();
        let mut contexts = retained::<T>(plan, workload, command.backend.validation_depth)?;
        if let Some(manifests) = manifests {
            contexts.shared = contexts
                .shared
                .checked_add(manifests.metadata_bytes())
                .ok_or(ConversionError("resource-budget"))?;
            contexts.leaf = contexts.leaf.max(manifests.minimum_bytes());
        }
        let hardware = Hardware::detect();
        #[cfg(test)]
        let hardware = self.hardware.unwrap_or(hardware);
        let budget = budget(
            hardware,
            workload,
            command.maximum_workers,
            command.backend.runner.native_capacity(),
            plan.outputs.len(),
            &contexts,
        )?;
        command.backend.runner.note_resource_budget(&budget);
        let bytes = contexts
            .worker
            .checked_mul(
                u64::try_from(budget.workers).map_err(|_| ConversionError("resource-budget"))?,
            )
            .and_then(|bytes| bytes.checked_add(contexts.shared))
            .ok_or(ConversionError("resource-budget"))?;
        let ceiling = command
            .backend
            .runner
            .native_capacity()
            .1
            .checked_sub(bytes)
            .filter(|ceiling| *ceiling >= contexts.leaf)
            .ok_or(ConversionError("resource-budget"))?;
        // Generated manifests have no reliable byte count before packaging. A
        // parent keeps all of its own remaining headroom for those dynamic claims;
        // its output/media workers still run in parallel. Known verification
        // graphs instead declare their actual minimum before context admission.
        let floor = if manifests.is_none()
            && plan
                .outputs
                .iter()
                .any(|output| output.format != OutputFormat::Mp4)
        {
            ceiling
        } else {
            contexts.leaf
        };
        let retained = command
            .backend
            .runner
            .retain_context(bytes, floor, &command.backend.stop, command.backend.timeout)
            .map_err(|error| match error {
                ReservationError::Cancelled => ConversionError("cancelled"),
                ReservationError::Insufficient | ReservationError::Timeout => {
                    ConversionError("resource-budget")
                }
            })?;
        let mut scoped = command.with_processing_clone(command.processing_mode);
        scoped.backend.runner =
            std::sync::Arc::new(command.backend.runner.within_context(bytes, floor));
        Ok((
            Prepared {
                command: scoped,
                budget,
                workload,
                #[cfg(test)]
                hardware: self.hardware,
            },
            retained,
        ))
    }
}

fn retained<T>(
    plan: &BundlePlan,
    workload: Workload,
    depth: crate::conversion::ValidationDepth,
) -> Result<Contexts, ConversionError> {
    let bad = ConversionError("resource-budget");
    let tracks = u64::try_from(workload.tracks.max(1)).map_err(|_| bad)?;
    let mut shared = frozen::retained_inventory_bytes(&plan.source)?
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(recipe_bytes(plan).ok()?))
        .ok_or(bad)?;
    for output in &plan.outputs {
        shared = shared
            .checked_add(frozen::retained_inventory_bound(
                output.format == OutputFormat::Mp4,
            )?)
            .ok_or(bad)?;
    }
    let result = std::mem::size_of::<T>()
        .checked_mul(4)
        .and_then(|bytes| bytes.checked_add(8 * 1024))
        .ok_or(bad)?;
    let metadata = plan.outputs.len().checked_mul(result).ok_or(bad)?;
    shared = shared
        .checked_add(u64::try_from(metadata).map_err(|_| bad)?)
        .and_then(|bytes| bytes.checked_add(tracks.checked_mul(TRACK_RESIDENCY_BYTES)?))
        .ok_or(bad)?;
    let leaf_workload = Workload {
        input_buffer_bytes: Some(
            workload
                .input_buffer_bytes
                .unwrap_or(32 * 1024 * 1024)
                .max(32 * 1024 * 1024),
        ),
        ..workload
    };
    // Decoded TS canonical probes can reserve the largest stdout capture. Use
    // that conservative bound for decoded work, including mixed output routes.
    // Native work never starts those probes and retains its existing estimate.
    let stdout_bytes = if depth == crate::conversion::ValidationDepth::Decoded {
        crate::toolrun::MAX_CAPTURE_BYTES
    } else {
        crate::toolrun::DEFAULT_STDOUT_BYTES
    };
    Ok(Contexts {
        shared,
        // Each pipeline can retain prepared source and candidate track contexts.
        worker: tracks.checked_mul(2 * TRACK_RESIDENCY_BYTES).ok_or(bad)?,
        // Native fMP4 rewrite is the largest fixed native claim. Tool working
        // sets retain the registered dimensions and at least the whole-file window.
        leaf: leaf_workload
            .estimated_child_bytes(stdout_bytes, crate::toolrun::DEFAULT_STDERR_BYTES)
            .max(NATIVE_LEAF_BYTES),
    })
}

fn recipe_bytes(plan: &BundlePlan) -> Result<u64, ConversionError> {
    let bad = ConversionError("resource-budget");
    let mut bytes = plan
        .outputs
        .capacity()
        .checked_mul(std::mem::size_of::<super::BundleOutputIntent>())
        .ok_or(bad)?;
    for output in &plan.outputs {
        bytes = bytes
            .checked_add(output.id.capacity())
            .and_then(|bytes| {
                bytes.checked_add(
                    output
                        .drms
                        .capacity()
                        .checked_mul(std::mem::size_of::<super::DrmSystem>())?,
                )
            })
            .ok_or(bad)?;
    }
    if let Some(processing) = &plan.processing {
        bytes = bytes
            .checked_add(
                processing
                    .capacity()
                    .checked_mul(std::mem::size_of::<super::OutputProcessing>())
                    .ok_or(bad)?,
            )
            .ok_or(bad)?;
        for choice in processing {
            bytes = bytes.checked_add(choice.id.capacity()).ok_or(bad)?;
        }
    }
    for size in [
        plan.kind.capacity(),
        plan.plan_id.capacity(),
        plan.output_root.capacity(),
    ] {
        bytes = bytes.checked_add(size).ok_or(bad)?;
    }
    u64::try_from(bytes).map_err(|_| bad)
}

fn budget(
    hardware: Hardware,
    workload: Workload,
    maximum: Option<usize>,
    capacity: (usize, u64),
    units: usize,
    contexts: &Contexts,
) -> Result<Budget, ConversionError> {
    let mut budget = Budget::choose(hardware, workload, maximum)
        .map_err(|_| ConversionError("resource-budget"))?;
    budget.cpu_tokens = budget.cpu_tokens.min(capacity.0);
    budget.memory_tokens_bytes = budget
        .memory_tokens_bytes
        .min(capacity.1)
        .saturating_sub(contexts.shared);
    budget.estimated_child_bytes = contexts
        .worker
        .checked_add(contexts.leaf)
        .ok_or(ConversionError("resource-budget"))?;
    budget.workers = budget.workers.min(units).min(budget.cpu_tokens).min(
        usize::try_from(budget.memory_tokens_bytes / budget.estimated_child_bytes)
            .unwrap_or(usize::MAX),
    );
    if budget.workers == 0 {
        return Err(ConversionError("resource-budget"));
    }
    Ok(budget)
}

#[cfg(test)]
mod tests;
