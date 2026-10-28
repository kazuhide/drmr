//! One complete terminal frame, containing current state rather than an event history.
use super::{Units, View, duration, estimate_duration, remaining, size};
use crate::progress::pipeline::{Phase, State, Status};
use std::time::Duration;

const LABEL_WIDTH: usize = 12;
const BAR_WIDTH: usize = 18;

#[cfg(test)]
mod tests;

mod items;

#[cfg(test)]
pub(super) fn frame(command: &str, view: &View<'_>, now: Duration, columns: usize) -> String {
    frame_with_height(command, view, now, columns, 24)
}

pub(super) fn frame_with_height(
    command: &str,
    view: &View<'_>,
    now: Duration,
    columns: usize,
    height: usize,
) -> String {
    if height == 0 {
        return String::new();
    }
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
            &progress_value(
                None,
                if view.activity == "acquisition" {
                    "Waiting for source"
                } else {
                    view.label(command)
                },
                overall_progress_width(columns),
            ),
            columns,
        ));
    } else {
        if view.downloads.is_some() {
            rows.push(row("Source", &source(view, now, columns), columns));
        }
        if view.activity != "acquisition" || view.downloads.is_none() {
            rows.push(row("Task", &task(command, view, now, columns), columns));
        }
    }
    items::finish(rows, view.pipeline, columns, height).join("\n")
}

fn overall(pipeline: &crate::progress::pipeline::State, now: Duration, columns: usize) -> String {
    let phase = pipeline.phase.expect("dashboard pipeline phase");
    let width = overall_progress_width(columns);
    let budget = detail_budget(columns, width);
    if let Some(estimate) = pipeline.estimate(now) {
        let progress = Some(bar(estimate.percent, true, columns));
        let mut value = "work".to_owned();
        if estimate.percent == 100 {
            append(&mut value, "Saved", budget);
            return progress_value(progress, &value, width);
        }
        if matches!(phase, Phase::FinalCheck | Phase::Publication) {
            append(&mut value, phase.label(), budget);
        }
        let eta = estimate.remaining.map_or_else(
            || "ETA estimating".to_owned(),
            |left| format!("ETA ~{}", estimate_duration(left)),
        );
        append(&mut value, &eta, budget);
        progress_value(progress, &value, width)
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
        progress_value(None, &value, width)
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
    let work = output.renditions();
    let units = output.units().filter(|_| work.is_none());
    let label_text = step_label(units, output.progress_label());
    let width = progress_width(columns);
    let budget = detail_budget(columns, width);
    let progress = (estimates && output.lifetime.status != Status::Aborted)
        .then(|| bar(output.estimated_percent(), true, columns));
    let mut value = if let Some(work) = work {
        rendition_value(&work, label_text, columns, budget)
    } else {
        label_text.to_owned()
    };
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
                // Comparison progress counts tracks without an explicit unit field.
                format!(
                    "{done}/{total} {}",
                    media.unit.map_or("tracks", |unit| unit.label())
                )
            } else if let Some([done, total]) = media.bytes {
                format!("{}/{}", size(done), size(total))
            } else {
                String::new()
            };
            append(&mut value, &counter, budget);
        } else if let Some(units) = units {
            append_units(&mut value, units, now, budget);
        }
    }
    row(&label, &progress_value(progress, &value, width), columns)
}

fn task(command: &str, view: &View<'_>, now: Duration, columns: usize) -> String {
    let units = view.units.filter(|_| view.activity != "acquisition");
    let label = if view.task.is_empty() {
        view.label(command)
    } else {
        view.task
    };
    let mut value = step_label(units, label).to_owned();
    if let Some(units) = units {
        append_units(&mut value, units, now, detail_budget(columns, 0));
    }
    value
}

fn source(view: &View<'_>, now: Duration, columns: usize) -> String {
    let width = progress_width(columns);
    let budget = detail_budget(columns, width);
    let work = view.pipeline.and_then(State::source_renditions);
    let context = view.pipeline.is_some_and(State::has_source_context);
    let status = view
        .pipeline
        .map(|pipeline| pipeline.lifetime(Phase::Source).status);
    let mut value = if let Some(work) = work {
        rendition_value(&work, work.label(false), columns, budget)
    } else {
        match status {
            Some(Status::Finished) => "Ready".to_owned(),
            Some(Status::Aborted) => "Stopped".to_owned(),
            Some(Status::Running) => "Checking".to_owned(),
            _ if view.activity == "configuration" => "Waiting".to_owned(),
            _ if view.activity == "acquisition" => "Downloading".to_owned(),
            _ => "Checking".to_owned(),
        }
    };
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
    if view.activity == "acquisition"
        && let Some(left) = view.units.and_then(|units| remaining(units, now))
    {
        append(
            &mut value,
            &format!("ETA ~{}", estimate_duration(left)),
            budget,
        );
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
        } else if let Some(units) = view.units.filter(|_| {
            !context
                && view.pipeline.is_none_or(State::source_running)
                && view.activity != "acquisition"
        }) {
            value = step_label(Some(units), &value).to_owned();
            append_units(&mut value, units, now, budget);
        }
    }
    let progress = view
        .units
        .filter(|units| {
            view.activity == "acquisition"
                && matches!(status, None | Some(Status::Pending))
                && units.total > 0
                && !units.checking
        })
        .map(|units| {
            let percent =
                u32::try_from(u128::from(units.completed) * 100 / u128::from(units.total))
                    .unwrap_or(100)
                    .min(100);
            bar(percent, false, columns)
        });
    progress_value(progress, &value, width)
}

fn rendition_value(
    work: &crate::progress::pipeline::renditions::Work,
    label: &str,
    columns: usize,
    budget: usize,
) -> String {
    let counts = work.counts(true);
    if columns < 60 || label.len() + counts.len() + 3 > budget {
        format!("{} {counts}", work.compact_label())
    } else {
        format!("{label} | {counts}")
    }
}

fn step_label(units: Option<Units>, fallback: &str) -> &str {
    if units.is_some_and(|units| units.checking) {
        "Checking boundaries"
    } else {
        fallback
    }
}

fn append_units(value: &mut String, units: Units, now: Duration, budget: usize) {
    let name = if units.segments { "segments" } else { "files" };
    append(
        value,
        &format!("{}/{} {name}", units.completed, units.total),
        budget,
    );
    if !units.checking && units.total > 0 {
        let percent = (u128::from(units.completed) * 100 / u128::from(units.total)).min(100);
        append(value, &format!("step {percent}%"), budget);
        if let Some(left) = remaining(units, now) {
            append(value, &format!("step ~{}", estimate_duration(left)), budget);
        }
    }
}

fn bar(percent: u32, approximate: bool, columns: usize) -> String {
    let number = format!("{}{percent}%", if approximate { "~" } else { "" });
    if columns < 60 {
        return number;
    }
    let cells = 10;
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
    let label = console::pad_str(
        label,
        label_width(columns),
        console::Alignment::Left,
        Some("..."),
    );
    fit(&format!("{label} {value}"), columns)
}

fn label_width(columns: usize) -> usize {
    if columns < 60 { 11 } else { LABEL_WIDTH }
}

fn progress_width(columns: usize) -> usize {
    if columns < 60 { 0 } else { BAR_WIDTH }
}

fn overall_progress_width(columns: usize) -> usize {
    if columns < 60 { 5 } else { BAR_WIDTH }
}

fn detail_budget(columns: usize, progress_width: usize) -> usize {
    columns
        .saturating_sub(label_width(columns) + 1 + progress_width + usize::from(progress_width > 0))
}

fn progress_value(progress: Option<String>, details: &str, width: usize) -> String {
    if width == 0 {
        details.to_owned()
    } else {
        format!("{:width$} {details}", progress.unwrap_or_default())
    }
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
