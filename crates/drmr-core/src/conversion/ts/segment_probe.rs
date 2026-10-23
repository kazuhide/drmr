//! Read bounded TS segment metadata, packet counts and codec settings in one probe.
use super::{Backend, ConversionError, View, canonical, tool_error};
use crate::{conversion::backend, toolrun::RunOptions};

const PROFILE_LIMIT: usize = 1024 * 1024;

impl Backend {
    pub(super) fn transport_segment_profile(
        &self,
        view: View<'_>,
    ) -> Result<(serde_json::Value, canonical::Config), ConversionError> {
        // Other views can have distinct initialization metadata, decryption
        // keys or edit-list rules. Their independent probes remain unchanged.
        if !view.transport || view.input != view.metadata || view.key.is_some() || view.unedited {
            return Err(ConversionError("ts-segment-probe-profile-not-supported"));
        }
        let argv = vec![
            self.ffprobe.clone(),
            "-v".into(),
            "error".into(),
            "-protocol_whitelist".into(),
            "file,crypto".into(),
            "-select_streams".into(),
            view.index.to_string(),
            "-count_packets".into(),
            "-show_entries".into(),
            format!(
                "stream={},extradata:{}",
                backend::PROFILE_FIELDS,
                backend::PROFILE_SIDE_DATA
            ),
            "-show_data_hash".into(),
            "sha256".into(),
            "-show_data".into(),
            "-of".into(),
            "json".into(),
            super::super::files::tool_path(view.input)?,
        ];
        let shown = ["ffprobe".into(), "[arguments redacted]".into()];
        let options = RunOptions {
            stop: Some(&self.stop),
            argv_redacted: Some(&shown),
            timeout: Some(self.timeout),
            max_stdout_bytes: PROFILE_LIMIT,
            max_output_chars: 0,
            log_stdout: false,
            ..RunOptions::default()
        };
        self.check_cancelled()?;
        let effective = Self::effective_argv(&argv, "ffprobe");
        let completed = self
            .runner
            .run_tool(&effective, "ffprobe", &options)
            .map_err(|error| tool_error(&error))?;
        self.check_cancelled()?;
        if completed.returncode != Some(0) {
            return Err(ConversionError("media-tool-failed"));
        }
        let value: serde_json::Value = serde_json::from_slice(&completed.stdout)
            .map_err(|_| ConversionError("invalid-media-probe"))?;
        let profile = backend::stream_profile(&value, view.index)?;
        self.runner
            .register_workload(super::super::execution::observe_profile(
                &profile, view.input,
            )?);
        let config = if profile["codec_name"] == "h264" {
            if profile["time_base"] != "1/90000" {
                return Err(ConversionError("ts-clock-timescale-not-supported"));
            }
            let data = value["streams"][0]["extradata"]
                .as_str()
                .ok_or(ConversionError("ts-codec-configuration-not-supported"))?;
            canonical::Config::video(&canonical::dump(data.as_bytes(), false)?, true)?
        } else {
            self.transport_config(view, &profile)?
        };
        Ok((profile, config))
    }
}

#[cfg(test)]
mod tests;
