//! Test-only native observation, exported with qualifiers rather than the runtime image.
fn main() {
    let hardware = drmr_core::resources::Hardware::detect();
    let memory_source = match hardware.memory_source {
        drmr_core::resources::MemorySource::Host => "host",
        drmr_core::resources::MemorySource::ProcessCgroup => "process-cgroup",
        drmr_core::resources::MemorySource::Unknown => "unknown",
    };
    println!(
        "{}",
        serde_json::json!({"schema_version":1, "kind":"native-hardware-observation",
            "effective_cpus":hardware.effective_cpus,
            "available_memory_bytes":hardware.available_memory_bytes,
            "memory_source":memory_source})
    );
}
