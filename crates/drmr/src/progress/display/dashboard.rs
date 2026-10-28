//! One complete terminal frame, containing current state rather than an event history.
use super::{View, duration, estimate_duration, remaining, size};
use crate::progress::pipeline::{Phase, Status};
use std::time::Duration;

const LABEL_WIDTH: usize = 12;

pub(super) fn frame(command: &str, view: &View<'_>, now: Duration, columns: usize) -> String {
    let columns = columns.clamp(1, 120);
    let title = format!("drmr {command}");
    let elapsed = format!("elapsed {}", duration(now));
    let mut rows = vec![fit(
        &format!(
            "{title}{} {elapsed}",
            " ".repeat(columns.saturating_sub(title.len() + elapsed.len() + 1))
        ),
        columns,
    )];
    if let Some(pipeline) = view.pipeline {
        rows.push(row("Source", &source(view, now, columns), columns));
        rows.push(row("Overall", &overall(pipeline, now, columns), columns));
        for (index, output) in pipeline.outputs().iter().enumerate() {
            rows.push(output_row(
                index,
                output,
                pipeline.estimates_enabled(),
                now,
                columns,
            ));
        }
    } else if command == "convert" {
        rows.push(row("Source", &source(view, now, columns), columns));
        rows.push(row(
            "Overall",
            if view.activity == "acquisition" {
                "Waiting for source"
            } else {
                view.label(command)
            },
            columns,
        ));
    } else {
        if view.downloads.is_some() {
            rows.push(row("Source", &source(view, now, columns), columns));
        }
        if view.activity != "acquisition" || view.downloads.is_none() {
            rows.push(row("Task", &task(command, view, columns), columns));
        }
    }
    rows.join("\n")
}

fn overall(pipeline: &crate::progress::pipeline::State, now: Duration, columns: usize) -> String {
    let phase = pipeline.phase.expect("dashboard pipeline phase");
    let budget = columns.saturating_sub(LABEL_WIDTH + 1);
    if let Some(estimate) = pipeline.estimate(now) {
        let mut value = format!("{} work", bar(estimate.percent, true, columns));
        if estimate.percent == 100 {
            append(&mut value, "Saved", budget);
            return value;
        }
        if matches!(phase, Phase::FinalCheck | Phase::Publication) {
            append(&mut value, phase.label(), budget);
        }
        let eta = estimate.remaining.map_or_else(
            || "ETA estimating".to_owned(),
            |left| format!("ETA ~{}", estimate_duration(left)),
        );
        append(&mut value, &eta, budget);
        value
    } else {
        let mut value = phase.label().to_owned();
        if phase == Phase::Outputs {
            append(
                &mut value,
                &format!(
                    "{}/{} checked",
                    pipeline.checked_outputs(),
                    pipeline.outputs().len()
                ),
                budget,
            );
        }
        value
    }
}

fn output_row(
    index: usize,
    output: &crate::progress::pipeline::Output,
    estimates: bool,
    now: Duration,
    columns: usize,
) -> String {
    let label = format!(
        "{} {}",
        index + 1,
        output
            .format
            .map_or("Output", crate::progress::pipeline::Format::label)
    );
    let mut value = if estimates && output.lifetime.status != Status::Aborted {
        format!(
            "{} {}",
            bar(output.estimated_percent(), true, columns),
            output.progress_label()
        )
    } else {
        output.progress_label().to_owned()
    };
    let budget = columns.saturating_sub(LABEL_WIDTH + 1);
    if output.lifetime.status == Status::Running {
        if estimates && let Some(left) = output.step_remaining(now) {
            append(
                &mut value,
                &format!("step ~{}", estimate_duration(left)),
                budget,
            );
        }
        if let Some(media) = output.media().filter(|media| media.active()) {
            let counter = if let Some([done, total]) = media.units {
                format!(
                    "{done}/{total} {}",
                    media.unit.map_or("items", |unit| unit.label())
                )
            } else if let Some([done, total]) = media.bytes {
                format!("{}/{}", size(done), size(total))
            } else {
                String::new()
            };
            append(&mut value, &counter, budget);
        } else if let Some(units) = output.units() {
            append(
                &mut value,
                &format!("{}/{} segments", units.completed, units.total),
                budget,
            );
        }
    }
    row(&label, &value, columns)
}

fn task(command: &str, view: &View<'_>, columns: usize) -> String {
    let mut value = if view.task.is_empty() {
        view.label(command)
    } else {
        view.task
    }
    .to_owned();
    if let Some(units) = view.units.filter(|_| view.activity != "acquisition") {
        let counter = if units.checking {
            "checking boundaries".to_owned()
        } else {
            format!("{}/{} segments", units.completed, units.total)
        };
        append(
            &mut value,
            &counter,
            columns.saturating_sub(LABEL_WIDTH + 1),
        );
    }
    value
}

fn source(view: &View<'_>, now: Duration, columns: usize) -> String {
    let status = view
        .pipeline
        .map(|pipeline| pipeline.lifetime(Phase::Source).status);
    let mut value = match status {
        Some(Status::Finished) => "Ready".to_owned(),
        Some(Status::Aborted) => "Stopped".to_owned(),
        Some(Status::Running) => "Checking".to_owned(),
        _ if view.activity == "configuration" => "Waiting".to_owned(),
        _ if view.activity == "acquisition" => {
            let mut text = "Downloading".to_owned();
            if let Some(units) = view
                .units
                .filter(|units| units.total > 0 && !units.checking)
            {
                let percent =
                    u32::try_from(u128::from(units.completed) * 100 / u128::from(units.total))
                        .unwrap_or(100)
                        .min(100);
                text = format!("{} Downloading", bar(percent, false, columns));
            }
            text
        }
        _ => "Checking".to_owned(),
    };
    let budget = columns.saturating_sub(LABEL_WIDTH + 1);
    if view.activity == "acquisition"
        && let Some(left) = view.units.and_then(|units| remaining(units, now))
    {
        append(
            &mut value,
            &format!("ETA ~{}", estimate_duration(left)),
            budget,
        );
    }
    if let Some([_, completed, bytes]) = view.downloads {
        if view.activity != "acquisition" || view.units.is_some() {
            let files = view
                .units
                .filter(|_| view.activity == "acquisition")
                .map_or_else(
                    || {
                        format!(
                            "{completed} {}",
                            if completed == 1 { "file" } else { "files" }
                        )
                    },
                    |units| format!("{completed}/{} files", units.total),
                );
            append(&mut value, &files, budget);
        }
        append(&mut value, &size(bytes), budget);
    }
    if status == Some(Status::Running) || view.downloads.is_none() {
        let media = view
            .pipeline
            .and_then(crate::progress::pipeline::State::source_media);
        if let Some(media) = media
            && let Some([done, total]) = media.units
        {
            append(
                &mut value,
                &format!(
                    "{done}/{total} {}",
                    media.unit.map_or("items", |unit| unit.label())
                ),
                budget,
            );
        } else if let Some(units) = view.units.filter(|_| view.activity != "acquisition") {
            let note = if units.checking {
                "checking boundaries".to_owned()
            } else {
                format!("{}/{} segments", units.completed, units.total)
            };
            append(&mut value, &note, budget);
        }
    }
    value
}

fn bar(percent: u32, approximate: bool, columns: usize) -> String {
    let number = format!("{}{percent}%", if approximate { "~" } else { "" });
    if columns < 60 {
        return number;
    }
    let cells = if columns >= 100 { 20 } else { 10 };
    let filled = usize::try_from(percent.min(100)).unwrap_or(100) * cells / 100;
    format!(
        "[{}{}] {number:>5}",
        "#".repeat(filled),
        "-".repeat(cells - filled)
    )
}

fn append(value: &mut String, part: &str, budget: usize) {
    if !part.is_empty() && value.len() + part.len() + 3 <= budget {
        value.push_str(" | ");
        value.push_str(part);
    }
}

fn row(label: &str, value: &str, columns: usize) -> String {
    fit(&format!("{label:<LABEL_WIDTH$} {value}"), columns)
}

fn fit(value: &str, columns: usize) -> String {
    console::truncate_str(value, columns, if columns >= 3 { "..." } else { "" })
        .trim_end()
        .to_owned()
}

pub(super) fn stopped_summary(view: &View<'_>) -> Vec<String> {
    let Some(pipeline) = view.pipeline else {
        return Vec::new();
    };
    let source = match pipeline.lifetime(Phase::Source).status {
        Status::Finished => "Checked",
        Status::Pending => "Not checked",
        Status::Running | Status::Aborted => "Stopped",
    };
    let mut rows = vec![row("Source", source, 78)];
    for (index, output) in pipeline.outputs().iter().enumerate() {
        let label = format!(
            "{} {}",
            index + 1,
            output
                .format
                .map_or("Output", crate::progress::pipeline::Format::label)
        );
        rows.push(row(
            &label,
            match output.lifetime.status {
                Status::Finished => "Checked",
                Status::Pending => "Not started",
                Status::Running | Status::Aborted => "Stopped",
            },
            78,
        ));
    }
    rows
}
