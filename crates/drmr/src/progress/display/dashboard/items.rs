//! Current task states only; completed batches never stand in for packaging work.
use super::{BAR_WIDTH, State, detail_budget, fit, progress_value, row};
use crate::progress::pipeline::renditions::{ItemState, Work};
use drmr_core::inspection::TrackKind;

struct Group {
    output: Option<usize>,
    work: Work,
    caption: &'static str,
}

struct Item<'a> {
    group: &'a Group,
    ordinal: usize,
    state: ItemState,
}

pub(super) fn finish(
    mut parents: Vec<String>,
    pipeline: Option<&State>,
    columns: usize,
    height: usize,
) -> Vec<String> {
    let groups = groups(pipeline);
    // The validated protocol bounds this table to eight outputs of 64 tasks.
    // Interleave by ordinal so a clipped table retains visibility across outputs.
    let mut items = Vec::new();
    for ordinal in 0..64 {
        for group in &groups {
            if let Some(state) = group.work.items().and_then(|items| items.get(ordinal)) {
                items.push(Item {
                    group,
                    ordinal,
                    state: *state,
                });
            }
        }
    }
    if parents.len() > height || (parents.len() == height && !items.is_empty()) {
        let keep = height.saturating_sub(1);
        let hidden_parents = parents.len().saturating_sub(keep);
        parents.truncate(keep);
        parents.push(summary(hidden_parents, &items, columns));
        return parents;
    }
    if items.is_empty() {
        return parents;
    }
    let available = height - parents.len();
    if items.len() < available {
        parents.push(fit("Renditions", columns));
        parents.extend(items.iter().map(|item| render(item, columns)));
    } else if available >= 3 {
        let visible = available - 2;
        parents.push(fit("Renditions", columns));
        parents.extend(items[..visible].iter().map(|item| render(item, columns)));
        parents.push(summary(0, &items[visible..], columns));
    } else {
        // Do not spend a scarce row on an orphaned table heading.
        parents.push(summary(0, &items, columns));
    }
    parents
}

fn groups(pipeline: Option<&State>) -> Vec<Group> {
    let Some(pipeline) = pipeline else {
        return Vec::new();
    };
    let mut groups = Vec::new();
    if let Some(work) = pipeline
        .source_renditions()
        .filter(|work| work.items().is_some())
    {
        groups.push(Group {
            output: None,
            work,
            caption: work.label(false),
        });
    }
    for (index, output) in pipeline.outputs().iter().enumerate() {
        if let Some(work) = output.renditions().filter(|work| work.items().is_some()) {
            groups.push(Group {
                output: Some(index),
                work,
                caption: output.detail(),
            });
        }
    }
    groups
}

fn render(item: &Item<'_>, columns: usize) -> String {
    let description = item.group.work.description(item.ordinal);
    let kind = match description.map(|description| description.kind) {
        Some(TrackKind::Video) => Some("Video"),
        Some(TrackKind::Audio) => Some("Audio"),
        Some(TrackKind::Text) => Some("Text"),
        _ => None,
    };
    let prefix = item
        .group
        .output
        .map_or_else(|| "S".to_owned(), |index| (index + 1).to_string());
    let ordinal = item.ordinal + 1;
    let label = kind.map_or_else(
        || format!("{prefix} #{ordinal}"),
        |kind| format!("{prefix} {kind} {ordinal}"),
    );
    let status = match item.state {
        ItemState::Waiting => "Waiting",
        ItemState::Active => "Active",
        ItemState::Completed => "Done (step)",
        ItemState::Stopped => "Stopped",
    };
    let width = if columns < 60 { 11 } else { BAR_WIDTH };
    let budget = detail_budget(columns, width);
    let dimensions = description
        .and_then(|description| description.resolution)
        .map(|[width, height]| format!("{width}x{height}"));
    let full = item.group.caption;
    let required = full.len() + dimensions.as_ref().map_or(0, |value| value.len() + 3);
    let mut details = if required <= budget {
        full
    } else {
        item.group.work.compact_label()
    }
    .to_owned();
    if let Some(dimensions) = dimensions {
        super::append(&mut details, &dimensions, budget);
    }
    row(
        &label,
        &progress_value(Some(status.to_owned()), &details, width),
        columns,
    )
}

fn summary(hidden_parents: usize, items: &[Item<'_>], columns: usize) -> String {
    let active = items
        .iter()
        .filter(|item| item.state == ItemState::Active)
        .count();
    let waiting = items
        .iter()
        .filter(|item| item.state == ItemState::Waiting)
        .count();
    let total = items.len();
    let full = if hidden_parents == 0 {
        format!("{total} hidden: {active} active, {waiting} waiting")
    } else if total == 0 {
        format!("{hidden_parents} rows hidden")
    } else {
        format!("{hidden_parents} rows hidden; {total} tasks: {active} active, {waiting} waiting")
    };
    let value = if full.len() > columns && hidden_parents > 0 && total > 0 {
        format!("{hidden_parents} rows; {total} hidden: {active} active/{waiting} wait")
    } else {
        full
    };
    fit(&value, columns)
}
