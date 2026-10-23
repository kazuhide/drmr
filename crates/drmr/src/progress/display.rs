//! Human progress is a current operation and a short history, not an event dump.
use std::io::Write;
use std::time::Duration;

use indicatif::{HumanBytes, ProgressBar};

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
    pub tools: &'a [&'static str],
    pub downloads: Option<[u64; 3]>,
    pub units: Option<Units>,
}

impl View<'_> {
    fn label(&self) -> &'static str {
        if self.units.is_some_and(|u| u.segments) {
            return "Preparing TS segments";
        }
        match self.activity {
            "configuration" => "Checking settings",
            "acquisition" => "Downloading source",
            "input-preparation" => "Preparing source",
            "inspection" => "Reading metadata",
            "normalization" | "packaging" => "Converting media",
            "verification" => "Verifying media",
            "publication" => "Saving result",
            "tool-check" => "Checking tools",
            "server-startup" => "Starting web console",
            "serving" => "Running web console",
            _ => "Working",
        }
    }

    fn details(&self, now: Duration, estimate: bool) -> String {
        let mut parts = Vec::new();
        if let Some(units) = self.units {
            let name = if units.segments { "segments" } else { "files" };
            parts.push(format!("{}/{} {name}", units.completed, units.total));
            if units.checking {
                parts.push("checking segment boundaries".to_owned());
            } else if units.total > 0 && estimate {
                let percent = (u128::from(units.completed) * 100) / u128::from(units.total);
                parts.push(format!("{percent}%"));
                if let Some(remaining) = remaining(units, now) {
                    parts.push(format!("~{} remaining in this step", duration(remaining)));
                }
            }
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
        } else if self.units.is_none() {
            if !self.task.is_empty() {
                parts.push(self.task.to_owned());
            }
            if !self.tools.is_empty() {
                parts.push(self.tools.join(", "));
            }
        }
        parts.join(" | ")
    }
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

fn duration(value: Duration) -> String {
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
}

impl Display {
    pub(super) fn render(
        &mut self,
        out: &mut dyn Write,
        terminal: Option<&ProgressBar>,
        command: &str,
        view: &View<'_>,
        now: Duration,
        outcome: Option<(&str, Option<&str>)>,
    ) -> std::io::Result<()> {
        let label = view.label();
        let changed = self.label != Some(label);
        let latest_details = view.details(now, false);
        if changed || outcome.is_some() {
            if let Some(previous) = self.label {
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
                line(out, terminal, &summary)?;
            }
            self.label = Some(label);
            self.began = now;
        }
        if let Some((event, error)) = outcome {
            if let Some(bar) = terminal {
                bar.disable_steady_tick();
                bar.finish_and_clear();
            }
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
        let details = view.details(now, true);
        self.last_details = latest_details;
        let message = format!(
            "{label}{} | elapsed {}",
            if details.is_empty() {
                String::new()
            } else {
                format!(" | {details}")
            },
            duration(now)
        );
        if let Some(bar) = terminal {
            bar.set_message(message);
        } else {
            let since = now.saturating_sub(self.last_output);
            // Tool starts and scope closures never override the human log cadence.
            let useful = self.last_printed != self.last_details;
            if changed
                || (useful && since >= Duration::from_secs(5))
                || since >= Duration::from_secs(15)
            {
                writeln!(out, "  {message}")?;
                self.last_output = now;
                self.last_printed.clone_from(&self.last_details);
            }
        }
        Ok(())
    }
}

fn line(out: &mut dyn Write, terminal: Option<&ProgressBar>, value: &str) -> std::io::Result<()> {
    if let Some(bar) = terminal {
        bar.println(value);
        Ok(())
    } else {
        writeln!(out, "{value}")
    }
}

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
            tools: &["ffprobe"],
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
            tools: &[],
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
            tools: &[],
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
}
