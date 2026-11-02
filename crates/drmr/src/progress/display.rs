//! Human progress is a current operation and a short history, not an event dump.
use std::io::Write;
use std::time::Duration;

use super::pipeline::{Phase, Status, renditions::Work};
use super::terminal::LiveTerminal;
use indicatif::HumanBytes;

mod dashboard;

#[derive(Clone, Copy, Debug)]
pub(super) struct Units {
    pub completed: u64,
    pub total: u64,
    pub initial: u64,
    pub began: Duration,
    pub segments: bool,
    pub checking: bool,
}

pub(super) struct View<'a> {
    pub activity: &'static str,
    pub task: &'a str,
    pub downloads: Option<[u64; 3]>,
    pub units: Option<Units>,
    pub pipeline: Option<&'a super::pipeline::State>,
}

impl View<'_> {
    fn label(&self, command: &str) -> &'static str {
        match self.activity {
            "configuration" => "Checking settings",
            "acquisition" => "Downloading source",
            "input-preparation" | "inspection" | "normalization" | "packaging" | "verification" => {
                match command {
                    "convert" => "Converting media",
                    "verify" => "Verifying media",
                    "analyze" | "plan" => "Analyzing source",
                    _ => "Working",
                }
            }
            "publication" => "Saving result",
            "tool-check" => "Checking tools",
            "server-startup" => "Starting web console",
            "serving" => "Running web console",
            _ => "Working",
        }
    }

    fn details(&self, now: Duration, estimate: bool) -> String {
        let mut parts = Vec::new();
        if self.activity != "acquisition" && !self.task.is_empty() {
            parts.push(self.task.to_owned());
        }
        if let Some(units) = self.units {
            parts.push(unit_details(units, now, estimate));
        }
        if self.activity == "acquisition" {
            if let Some([_, completed, bytes]) = self.downloads {
                if self.units.is_none() {
                    parts.push(format!("{completed} files received"));
                }
                parts.push(format!("{} received", size(bytes)));
            } else {
                parts.push("discovering resources".to_owned());
            }
        }
        parts.join(" | ")
    }
}

fn unit_details(units: Units, now: Duration, estimate: bool) -> String {
    let name = if units.segments { "segments" } else { "files" };
    let mut value = format!("{}/{} {name}", units.completed, units.total);
    if units.checking {
        value.push_str(" | checking segment boundaries");
    } else if units.total > 0 && estimate {
        use std::fmt::Write as _;
        let percent = (u128::from(units.completed) * 100) / u128::from(units.total);
        write!(value, " | {percent}%").expect("string write");
        if let Some(remaining) = remaining(units, now) {
            write!(value, " | ~{} remaining in this step", duration(remaining))
                .expect("string write");
        }
    }
    value
}

fn remaining(units: Units, now: Duration) -> Option<Duration> {
    let elapsed = now.saturating_sub(units.began);
    let done = units.completed.saturating_sub(units.initial);
    if units.checking
        || done < 2
        || elapsed < Duration::from_secs(1)
        || units.completed >= units.total
    {
        return None;
    }
    let nanos = elapsed
        .as_nanos()
        .checked_mul(u128::from(units.total - units.completed))?
        / u128::from(done);
    let seconds = u64::try_from(nanos.div_ceil(1_000_000_000)).ok()?;
    // Unbounded or very remote estimates are less useful than the actual count.
    (seconds <= 7 * 86400).then(|| Duration::from_secs(seconds))
}

pub(crate) fn duration(value: Duration) -> String {
    let seconds = value.as_secs();
    if seconds >= 3600 {
        format!("{}h {:02}m", seconds / 3600, seconds / 60 % 60)
    } else if seconds >= 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{:.1}s", value.as_secs_f64())
    }
}

fn size(bytes: u64) -> String {
    HumanBytes(bytes).to_string()
}

#[derive(Default)]
pub(super) struct Display {
    label: Option<&'static str>,
    began: Duration,
    last_output: Duration,
    last_details: String,
    last_printed: String,
    last_paint: Option<Duration>,
    terminal_active: bool,
    pipeline_phase: Option<Phase>,
    source_renditions_printed: bool,
    pipeline_phases_printed: [bool; 4],
    pipeline_outputs_printed: [bool; 8],
}

impl Display {
    fn paint_due(&mut self, now: Duration) -> bool {
        if self
            .last_paint
            .is_none_or(|last| now.saturating_sub(last) >= Duration::from_secs(1))
        {
            self.last_paint = Some(now);
            true
        } else {
            false
        }
    }

    pub(super) fn render(
        &mut self,
        out: &mut dyn Write,
        terminal: Option<&LiveTerminal>,
        command: &str,
        view: &View<'_>,
        now: Duration,
        outcome: Option<(&str, Option<&str>)>,
    ) -> std::io::Result<()> {
        if terminal.is_some() || self.terminal_active {
            return self.render_dashboard(out, terminal, command, view, now, outcome);
        }
        if view.pipeline.is_some() {
            return self.render_pipeline(out, command, view, now, outcome);
        }
        let label = view.label(command);
        let changed = self.label != Some(label);
        let latest_details = view.details(now, false);
        if changed || outcome.is_some() {
            if let Some(previous) = self
                .label
                .filter(|_| outcome.is_none_or(|(event, _)| event == "finished"))
            {
                let details = if changed {
                    &self.last_details
                } else {
                    &latest_details
                };
                let summary = format!(
                    "  {previous:<22} {:>7}{}",
                    duration(now.saturating_sub(self.began)),
                    if details.is_empty() {
                        String::new()
                    } else {
                        format!("  {details}")
                    }
                );
                writeln!(out, "{summary}")?;
            }
            self.label = Some(label);
            self.began = now;
        }
        if let Some((event, error)) = outcome {
            let status = match event {
                "finished" => "Finished",
                "cancelled" => "Cancelled",
                _ => "Failed",
            };
            return writeln!(
                out,
                "{status} {command} in {}{}",
                duration(now),
                error.map_or_else(String::new, |code| format!(" | {code}"))
            );
        }
        self.last_details = latest_details;
        let details = view.details(now, true);
        let message = format!(
            "{label}{} | elapsed {}",
            if details.is_empty() {
                String::new()
            } else {
                format!(" | {details}")
            },
            duration(now)
        );
        let since = now.saturating_sub(self.last_output);
        // Tool starts and scope closures never override the human log cadence.
        let useful = self.last_printed != self.last_details;
        let complete = view.units.is_some_and(|units| {
            units.total > 0 && units.completed == units.total && !units.checking
        });
        if changed
            || (useful && complete)
            || (useful && since >= Duration::from_secs(5))
            || since >= Duration::from_secs(15)
        {
            writeln!(out, "  {message}")?;
            self.last_output = now;
            self.last_printed.clone_from(&self.last_details);
        }
        Ok(())
    }

    fn render_dashboard(
        &mut self,
        out: &mut dyn Write,
        terminal: Option<&LiveTerminal>,
        command: &str,
        view: &View<'_>,
        now: Duration,
        outcome: Option<(&str, Option<&str>)>,
    ) -> std::io::Result<()> {
        let painted = self.terminal_active;
        self.terminal_active = true;
        if let Some((event, error)) = outcome {
            if let Some(terminal) = terminal {
                terminal.close(out)?;
            }
            // A result can end in LF without the terminal driver mapping it to
            // CRLF. Start the footer at column zero without erasing that result.
            out.write_all(if painted { b"\r" } else { b"\r\n" })?;
            let status = match event {
                "finished" => "Finished",
                "cancelled" => "Cancelled",
                _ => "Failed",
            };
            write!(
                out,
                "{status} {command} in {}{}\r\n",
                duration(now),
                error.map_or_else(String::new, |code| format!(" | {code}"))
            )?;
            if event != "finished" {
                for line in dashboard::stopped_summary(view) {
                    write!(out, "{line}\r\n")?;
                }
            }
            return Ok(());
        }
        let Some(terminal) = terminal else {
            return Ok(());
        };
        // Sample the latest state, including phase changes, at a readable cadence.
        if self.paint_due(now) {
            let size = terminal.size();
            let (height, columns) = size;
            let columns = usize::from(columns.saturating_sub(1));
            let height = usize::from(height.saturating_sub(1));
            // A complete frame has one writer and one draw, including all outputs.
            terminal.draw_at_size(
                out,
                &dashboard::frame_with_height(command, view, now, columns, height),
                size,
            )?;
        }
        Ok(())
    }
}

impl Display {
    fn render_pipeline(
        &mut self,
        out: &mut dyn Write,
        command: &str,
        view: &View<'_>,
        now: Duration,
        outcome: Option<(&str, Option<&str>)>,
    ) -> std::io::Result<()> {
        let pipeline = view
            .pipeline
            .expect("pipeline view requires observed stages");
        let phase = pipeline
            .phase
            .expect("pipeline view requires an observed phase");
        self.pipeline_history(out, pipeline, now)?;
        if let Some((event, error)) = outcome {
            let status = match event {
                "finished" => "Finished",
                "cancelled" => "Cancelled",
                _ => "Failed",
            };
            return writeln!(
                out,
                "{status} {command} in {}{}",
                duration(now),
                error.map_or_else(String::new, |code| format!(" | {code}"))
            );
        }
        let changed = self.pipeline_phase != Some(phase);
        self.pipeline_phase = Some(phase);
        if changed && phase == Phase::Source {
            self.source_renditions_printed = false;
        }
        let source_units = view
            .units
            .filter(|_| pipeline.source_running() && !pipeline.has_source_context());
        let message = pipeline_message(pipeline, source_units, now);
        let snapshot = if phase == Phase::Source {
            let details = source_details(pipeline);
            if details.is_empty() {
                source_units.map_or_else(String::new, |units| unit_details(units, now, false))
            } else {
                details
            }
        } else {
            output_details(pipeline, now, false)
        };
        let source_started = pipeline.source_running()
            && pipeline.source_renditions().is_some_and(Work::has_started);
        let first_source = !self.source_renditions_printed && source_started;
        let since = now.saturating_sub(self.last_output);
        if changed
            || first_source
            || (snapshot != self.last_printed && since >= Duration::from_secs(5))
            || since >= Duration::from_secs(15)
        {
            writeln!(out, "  {message}")?;
            if phase != Phase::Source && !snapshot.is_empty() {
                writeln!(out, "{}", output_details(pipeline, now, true))?;
            }
            self.source_renditions_printed |= source_started;
            self.last_output = now;
            self.last_printed = snapshot;
        }
        Ok(())
    }

    fn pipeline_history(
        &mut self,
        out: &mut dyn Write,
        pipeline: &super::pipeline::State,
        now: Duration,
    ) -> std::io::Result<()> {
        for phase in Phase::ALL {
            let life = pipeline.lifetime(phase);
            let printed = &mut self.pipeline_phases_printed[phase.index()];
            if !*printed && life.status.terminal() {
                if phase != Phase::Outputs || pipeline.outputs().is_empty() {
                    let label = if life.status == Status::Finished {
                        phase.completed().to_owned()
                    } else {
                        format!("{} stopped", phase.label())
                    };
                    writeln!(
                        out,
                        "  {label:<22} {:>7}",
                        duration(life.elapsed(now).unwrap_or_default())
                    )?;
                }
                *printed = true;
            }
        }
        for (index, output) in pipeline.outputs().iter().enumerate() {
            if !self.pipeline_outputs_printed[index] && output.lifetime.status.terminal() {
                let format = output
                    .format
                    .map_or("Media", super::pipeline::Format::label);
                writeln!(
                    out,
                    "  Output {} · {format}: {} | {}",
                    index + 1,
                    output.detail(),
                    duration(output.lifetime.elapsed(now).unwrap_or_default())
                )?;
                self.pipeline_outputs_printed[index] = true;
            }
        }
        Ok(())
    }
}

fn pipeline_message(
    pipeline: &super::pipeline::State,
    source_units: Option<Units>,
    now: Duration,
) -> String {
    use std::fmt::Write as _;
    let phase = pipeline.phase.expect("pipeline message requires a phase");
    let source_units =
        source_units.filter(|_| pipeline.source_running() && !pipeline.has_source_context());
    let mut message = phase.label().to_owned();
    let estimate = pipeline.estimate(now);
    let source = source_details(pipeline);
    if let Some(estimate) = estimate {
        write!(message, " | ~{}% overall work", estimate.percent).expect("string write");
        if let Some(remaining) = estimate.remaining {
            write!(
                message,
                " | ~{} remaining (est.)",
                estimate_duration(remaining)
            )
            .expect("string write");
        } else if estimate.percent < 100 {
            message.push_str(" | estimating remaining time");
        }
        message.push_str("\n  ");
        if phase == Phase::Outputs && !pipeline.outputs().is_empty() {
            write!(
                message,
                "{}/{} checked | {} active | ",
                pipeline.checked_outputs(),
                pipeline.outputs().len(),
                pipeline.active_outputs()
            )
            .expect("string write");
        }
        write!(message, "elapsed {}", duration(now)).expect("string write");
    } else {
        if let Some(units) = source_units.filter(|_| source.is_empty()) {
            write!(message, " | {}", unit_details(units, now, true)).expect("string write");
        }
        if phase == Phase::Outputs && !pipeline.outputs().is_empty() {
            write!(
                message,
                " | {}/{} checked | {} in progress",
                pipeline.checked_outputs(),
                pipeline.outputs().len(),
                pipeline.active_outputs()
            )
            .expect("string write");
        }
        write!(message, " | elapsed {}", duration(now)).expect("string write");
    }
    if !source.is_empty() {
        write!(message, "\n  {source}").expect("string write");
    } else if estimate.is_some()
        && let Some(units) = source_units
    {
        write!(message, "\n  {}", unit_details(units, now, false)).expect("string write");
    }
    message
}

fn source_details(pipeline: &super::pipeline::State) -> String {
    use std::fmt::Write as _;
    if !pipeline.source_running() {
        return String::new();
    }
    let work = pipeline.source_renditions();
    let mut details = work.map_or_else(String::new, |work| {
        format!("{} | {}", work.label(false), work.counts(true))
    });
    if let Some(media) = pipeline.source_media()
        && let Some([done, total]) = media.units
    {
        if !details.is_empty() {
            details.push_str("\n  ");
        }
        let unit = media.unit.map_or("items", |unit| unit.label());
        write!(details, "{done}/{total} source {unit} prepared").expect("string write");
    }
    details
}

fn estimate_duration(value: Duration) -> String {
    let seconds = value
        .as_secs()
        .saturating_add(u64::from(value.subsec_nanos() > 0));
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        duration(Duration::from_secs(seconds))
    }
}

fn output_details(pipeline: &super::pipeline::State, now: Duration, estimate: bool) -> String {
    use std::fmt::Write as _;
    let mut result = String::new();
    for (index, output) in pipeline.outputs().iter().enumerate() {
        if output.lifetime.status.terminal() {
            continue;
        }
        if !result.is_empty() {
            result.push('\n');
        }
        let format = output
            .format
            .map_or("Media", super::pipeline::Format::label);
        write!(
            result,
            "    Output {} · {format}: {}",
            index + 1,
            output.detail()
        )
        .expect("string write");
        if pipeline.estimates_enabled() {
            write!(result, " | ~{}% work", output.estimated_percent()).expect("string write");
        }
        let work = output.renditions();
        if let Some(work) = work {
            write!(result, "\n      {}", work.counts(false)).expect("string write");
        }
        if let Some(media) = output.media().filter(|media| media.active()) {
            use super::pipeline::Task;
            match media.task {
                Task::Packaging => write!(
                    result,
                    "\n      {} / ~{} generated",
                    size(media.bytes.expect("packaging byte counters")[0]),
                    size(media.bytes.expect("packaging byte counters")[1])
                ),
                Task::Preparation => {
                    let [done, total] = media.units.expect("preparation unit counters");
                    let unit = media.unit.expect("preparation unit label").label();
                    write!(result, "\n      {done}/{total} {unit} prepared")
                }
                Task::Integrity => {
                    let [done, total] = media.units.expect("integrity file counters");
                    write!(result, "\n      {done}/{total} files checked")
                }
                Task::Comparison => {
                    if let Some([done, total]) = media.units {
                        write!(result, "\n      {done}/{total} tracks compared")
                    } else {
                        write!(
                            result,
                            "\n      {} / {} comparison work",
                            size(media.bytes.expect("comparison byte counters")[0]),
                            size(media.bytes.expect("comparison byte counters")[1])
                        )
                    }
                }
            }
            .expect("string write");
            if estimate
                && pipeline.estimates_enabled()
                && let Some(remaining) = output.step_remaining(now)
            {
                write!(
                    result,
                    " | ~{} for current task (est.)",
                    estimate_duration(remaining)
                )
                .expect("string write");
            }
        } else if let Some(units) = output.units().filter(|_| work.is_none()) {
            write!(result, "\n      {}", unit_details(units, now, estimate)).expect("string write");
        }
    }
    result
}

pub(super) fn notice(out: &mut dyn Write, value: &'static str) -> std::io::Result<()> {
    writeln!(out, "{value}")
}

#[cfg(test)]
mod estimate_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_use_only_completed_units_since_the_known_total() {
        let units = Units {
            completed: 30,
            total: 100,
            initial: 20,
            began: Duration::from_secs(5),
            segments: false,
            checking: false,
        };
        assert_eq!(
            remaining(units, Duration::from_secs(10)),
            Some(Duration::from_secs(35))
        );
        assert!(
            remaining(
                Units {
                    completed: 21,
                    ..units
                },
                Duration::from_secs(10)
            )
            .is_none()
        );
        assert!(
            remaining(
                Units {
                    completed: 100,
                    ..units
                },
                Duration::from_secs(10)
            )
            .is_none()
        );
        assert!(
            remaining(
                Units {
                    checking: true,
                    ..units
                },
                Duration::from_secs(10)
            )
            .is_none()
        );
    }

    #[test]
    fn unknown_work_has_no_percentage_or_estimate_and_history_has_real_duration() {
        let mut display = Display::default();
        let mut out = Vec::new();
        let mut view = View {
            activity: "inspection",
            task: "Reading track metadata",
            pipeline: None,
            downloads: None,
            units: None,
        };
        for second in 0..15 {
            display
                .render(
                    &mut out,
                    None,
                    "analyze",
                    &view,
                    Duration::from_secs(second),
                    None,
                )
                .unwrap();
        }
        assert_eq!(String::from_utf8_lossy(&out).lines().count(), 1);
        view.activity = "verification";
        display
            .render(
                &mut out,
                None,
                "analyze",
                &view,
                Duration::from_secs(15),
                None,
            )
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("15.0s"));
        assert!(!text.contains('%') && !text.contains("remaining"));
    }

    #[test]
    fn collecting_all_segments_does_not_complete_the_boundary_check() {
        let view = View {
            activity: "input-preparation",
            task: "",
            pipeline: None,
            downloads: None,
            units: Some(Units {
                completed: 10,
                total: 10,
                initial: 0,
                began: Duration::ZERO,
                segments: true,
                checking: true,
            }),
        };
        let text = view.details(Duration::from_secs(5), true);
        assert!(text.contains("10/10 segments") && text.contains("checking segment boundaries"));
        assert!(!text.contains("100%") && !text.contains("remaining"));
    }

    #[test]
    fn final_history_uses_the_latest_count_even_without_an_intermediate_line() {
        let mut display = Display::default();
        let mut out = Vec::new();
        let mut view = View {
            activity: "acquisition",
            task: "",
            pipeline: None,
            downloads: Some([10, 1, 100]),
            units: Some(Units {
                completed: 1,
                total: 10,
                initial: 0,
                began: Duration::ZERO,
                segments: false,
                checking: false,
            }),
        };
        display
            .render(&mut out, None, "analyze", &view, Duration::ZERO, None)
            .unwrap();
        view.downloads = Some([10, 10, 1000]);
        view.units.as_mut().unwrap().completed = 10;
        display
            .render(
                &mut out,
                None,
                "analyze",
                &view,
                Duration::from_secs(1),
                Some(("finished", None)),
            )
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("10/10 files"), "{text}");
        assert!(text.ends_with("Finished analyze in 1.0s\n"));
    }

    fn pipeline_event(
        pipeline: &mut super::super::pipeline::State,
        value: serde_json::Value,
        second: u64,
    ) {
        let serde_json::Value::Object(record) = value else {
            panic!("expected a pipeline event object");
        };
        assert!(pipeline.observe(&record, Duration::from_secs(second)));
    }

    #[test]
    fn source_units_remain_visible_only_during_the_source_phase() {
        let mut pipeline = super::super::pipeline::State::default();
        pipeline_event(
            &mut pipeline,
            serde_json::json!({"scope":"source","state":"started"}),
            0,
        );
        let mut display = Display::default();
        let mut out = Vec::new();
        let units = Units {
            completed: 2,
            total: 10,
            initial: 0,
            began: Duration::ZERO,
            segments: true,
            checking: false,
        };
        let render = |display: &mut Display,
                      out: &mut Vec<u8>,
                      pipeline: &super::super::pipeline::State,
                      second| {
            display
                .render(
                    out,
                    None,
                    "convert",
                    &View {
                        activity: "input-preparation",
                        task: "",
                        downloads: None,
                        units: Some(units),
                        pipeline: Some(pipeline),
                    },
                    Duration::from_secs(second),
                    None,
                )
                .unwrap();
        };
        render(&mut display, &mut out, &pipeline, 2);
        let source = String::from_utf8_lossy(&out);
        assert!(
            source.contains("Checking source | 2/10 segments | 20% | ~8.0s remaining in this step"),
            "{source}"
        );
        let prior = out.len();
        pipeline_event(
            &mut pipeline,
            serde_json::json!({"scope":"source","state":"finished"}),
            3,
        );
        pipeline_event(
            &mut pipeline,
            serde_json::json!({"scope":"outputs","state":"started"}),
            3,
        );
        render(&mut display, &mut out, &pipeline, 3);
        let later = String::from_utf8_lossy(&out[prior..]);
        assert!(
            later.contains("Source checked") && later.contains("Creating outputs"),
            "{later}"
        );
        assert!(
            !later.contains("2/10") && !later.contains("remaining"),
            "{later}"
        );
    }

    #[test]
    fn parallel_outputs_have_distinct_rows_and_aborted_work_is_never_checked() {
        let mut pipeline = super::super::pipeline::State::default();
        pipeline_event(
            &mut pipeline,
            serde_json::json!({"scope":"outputs","state":"started"}),
            0,
        );
        for index in 0..2 {
            pipeline_event(
                &mut pipeline,
                serde_json::json!({"scope":"output","state":"started","output_index":index,"output_total":2,"output_format":"hls"}),
                1,
            );
        }
        let mut display = Display::default();
        let mut out = Vec::new();
        let render = |display: &mut Display,
                      out: &mut Vec<u8>,
                      pipeline: &super::super::pipeline::State,
                      second,
                      outcome| {
            display
                .render(
                    out,
                    None,
                    "convert",
                    &View {
                        activity: "input-preparation",
                        task: "",
                        downloads: None,
                        units: None,
                        pipeline: Some(pipeline),
                    },
                    Duration::from_secs(second),
                    outcome,
                )
                .unwrap();
        };
        render(&mut display, &mut out, &pipeline, 2, None);
        let active = String::from_utf8_lossy(&out);
        assert!(active.contains("0/2 checked | 2 in progress"), "{active}");
        assert!(
            active.contains("Output 1 · HLS fMP4: Working")
                && active.contains("Output 2 · HLS fMP4: Working"),
            "{active}"
        );
        assert!(
            !active.contains('%') && !active.contains("remaining"),
            "{active}"
        );
        pipeline_event(
            &mut pipeline,
            serde_json::json!({"scope":"output","state":"finished","output_index":0,"output_total":2,"output_format":"hls"}),
            3,
        );
        pipeline_event(
            &mut pipeline,
            serde_json::json!({"scope":"output","state":"aborted","output_index":1,"output_total":2,"output_format":"hls"}),
            4,
        );
        pipeline_event(
            &mut pipeline,
            serde_json::json!({"scope":"outputs","state":"aborted"}),
            4,
        );
        for _ in 0..2 {
            render(&mut display, &mut out, &pipeline, 4, None);
        }
        render(
            &mut display,
            &mut out,
            &pipeline,
            5,
            Some(("failed", Some("source-invalid"))),
        );
        let text = String::from_utf8(out).unwrap();
        assert_eq!(
            text.matches("Output 1 · HLS fMP4: Checked").count(),
            1,
            "{text}"
        );
        assert_eq!(
            text.matches("Output 2 · HLS fMP4: Stopped").count(),
            1,
            "{text}"
        );
        assert!(
            !text.contains("Output 2 · HLS fMP4: Checked")
                && !text.contains("Outputs checked")
                && !text.contains("Finished"),
            "{text}"
        );
        assert!(
            text.ends_with("Failed convert in 5.0s | source-invalid\n"),
            "{text}"
        );
    }

    #[test]
    fn fallback_keeps_a_command_header_and_live_detail_without_microstage_history() {
        for (command, label) in [
            ("convert", "Converting media"),
            ("verify", "Verifying media"),
            ("analyze", "Analyzing source"),
            ("plan", "Analyzing source"),
        ] {
            let mut display = Display::default();
            let mut out = Vec::new();
            let mut view = View {
                activity: "input-preparation",
                task: "Reading metadata",
                downloads: None,
                units: None,
                pipeline: None,
            };
            display
                .render(&mut out, None, command, &view, Duration::ZERO, None)
                .unwrap();
            for iteration in 0..100 {
                view.task = if iteration % 2 == 0 {
                    "Reading metadata"
                } else {
                    "Checking media"
                };
                display
                    .render(
                        &mut out,
                        None,
                        command,
                        &view,
                        Duration::from_millis(iteration),
                        None,
                    )
                    .unwrap();
            }
            display
                .render(&mut out, None, command, &view, Duration::from_secs(5), None)
                .unwrap();
            display
                .render(
                    &mut out,
                    None,
                    command,
                    &view,
                    Duration::from_secs(6),
                    Some(("finished", None)),
                )
                .unwrap();
            let text = String::from_utf8(out).unwrap();
            assert!(
                text.starts_with(&format!("  {label} | Reading metadata")),
                "{text}"
            );
            assert!(
                text.contains(&format!("  {label} | Checking media")),
                "{text}"
            );
            assert!(text.lines().count() <= 4, "{text}");
            assert!(
                !text.contains("Preparing source")
                    && !text.contains('%')
                    && !text.contains("remaining"),
                "{text}"
            );
            assert!(
                !text.contains("Reading metadata        0.0s")
                    && !text.contains("Checking media         0.0s"),
                "{text}"
            );
        }
    }
}
