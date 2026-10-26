//! New output defaults are distinct from immutable saved-plan execution.
use drmr_core::{
    conversion::{ConversionError, Converter, dash_addressing::DashAddressing},
    inspection::OutputFormat,
};

pub fn configure(command: Converter, addressing: Option<DashAddressing>) -> Converter {
    command
        .with_new_output_defaults()
        .with_dash_addressing(addressing.unwrap_or(DashAddressing::TemplateDuration))
}

pub fn validate(
    addressing: Option<DashAddressing>,
    formats: impl IntoIterator<Item = OutputFormat>,
) -> Result<(), ConversionError> {
    if addressing.is_some()
        && !formats
            .into_iter()
            .any(|format| format == OutputFormat::Dash)
    {
        return Err(ConversionError("dash-addressing-requires-dash"));
    }
    Ok(())
}
