// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-step timings for BuildKit builds.
//!
//! BuildKit reports every build step as a *vertex* with its own start and
//! completion timestamps and a `cached` flag. The build log already prints
//! `[DONE]`/`[CACHED]` as each vertex finishes, but nothing tells the user
//! where the time went: whether a slow deploy spent it installing
//! dependencies, compiling, or exporting the image, and whether the cache
//! was hit at all. [`BuildStepTimer`] collects those vertices while the
//! build streams and renders a short summary for the end of the build log.
//!
//! Durations come from BuildKit's own timestamps, not from when the status
//! message reached Temps, so they are unaffected by log streaming latency.

use std::collections::HashMap;
use std::time::Duration;

/// Upper bound on tracked vertices. A build has one vertex per Dockerfile
/// instruction plus a handful of internal ones, so this is never reached by
/// a real build; it only keeps a pathological Dockerfile from growing the
/// map without bound.
const MAX_TRACKED_STEPS: usize = 2048;

/// Executed steps listed individually in the summary, slowest first.
const SUMMARY_SLOWEST_STEPS: usize = 8;

/// Step names longer than this are cut in the summary. `RUN --mount=...`
/// lines are long and the interesting part is at the start.
const SUMMARY_NAME_MAX_CHARS: usize = 110;

/// A BuildKit timestamp as `(seconds, nanos)` since the Unix epoch.
pub type VertexTimestamp = (i64, i32);

/// The timing of one build step, as reported by BuildKit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildStepTiming {
    /// The step as BuildKit names it, e.g. `[build 5/9] RUN npm ci`.
    pub name: String,
    /// The step's result came from the build cache.
    pub cached: bool,
    /// The step reported an error.
    pub failed: bool,
    /// Time between the step starting and completing. `None` while the step
    /// has not completed.
    pub duration: Option<Duration>,
}

#[derive(Debug, Default)]
struct StepRecord {
    name: String,
    cached: bool,
    failed: bool,
    started: Option<VertexTimestamp>,
    completed: Option<VertexTimestamp>,
}

/// Collects BuildKit vertex updates for one build.
#[derive(Debug, Default)]
pub struct BuildStepTimer {
    steps: Vec<StepRecord>,
    index: HashMap<String, usize>,
}

impl BuildStepTimer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one vertex update. BuildKit sends the same vertex several times
    /// as it progresses; later updates fill in what earlier ones lacked.
    pub fn observe(
        &mut self,
        digest: &str,
        name: &str,
        cached: bool,
        started: Option<VertexTimestamp>,
        completed: Option<VertexTimestamp>,
        error: &str,
    ) {
        if digest.is_empty() || name.is_empty() {
            return;
        }
        let position = match self.index.get(digest) {
            Some(&position) => position,
            None => {
                if self.steps.len() >= MAX_TRACKED_STEPS {
                    return;
                }
                self.steps.push(StepRecord {
                    name: name.to_string(),
                    ..StepRecord::default()
                });
                self.index.insert(digest.to_string(), self.steps.len() - 1);
                self.steps.len() - 1
            }
        };
        let step = &mut self.steps[position];
        step.cached |= cached;
        step.failed |= !error.is_empty();
        // The earliest start and latest completion win: a vertex can be
        // reported as started again after a retry.
        if let Some(started) = started {
            step.started = Some(step.started.map_or(started, |s| s.min(started)));
        }
        if let Some(completed) = completed {
            step.completed = Some(step.completed.map_or(completed, |c| c.max(completed)));
        }
    }

    /// Record every vertex in one BuildKit status message.
    pub fn observe_vertices(&mut self, vertexes: &[bollard::moby::buildkit::v1::Vertex]) {
        for vertex in vertexes {
            self.observe(
                &vertex.digest,
                &vertex.name,
                vertex.cached,
                vertex.started.as_ref().map(|t| (t.seconds, t.nanos)),
                vertex.completed.as_ref().map(|t| (t.seconds, t.nanos)),
                &vertex.error,
            );
        }
    }

    /// Every step seen so far, in the order BuildKit first reported them.
    pub fn steps(&self) -> Vec<BuildStepTiming> {
        self.steps
            .iter()
            .map(|step| BuildStepTiming {
                name: step.name.clone(),
                cached: step.cached,
                failed: step.failed,
                duration: match (step.started, step.completed) {
                    (Some(started), Some(completed)) => Some(elapsed(started, completed)),
                    _ => None,
                },
            })
            .collect()
    }

    /// A human-readable summary for the end of the build log, or `None` when
    /// no steps were reported (the legacy builder sends no vertices).
    pub fn summary(&self, wall: Duration) -> Option<String> {
        let steps = self.steps();
        if steps.is_empty() {
            return None;
        }

        let cached = steps.iter().filter(|s| s.cached).count();
        let mut executed: Vec<&BuildStepTiming> = steps
            .iter()
            .filter(|s| !s.cached && s.duration.is_some())
            .collect();
        executed.sort_by_key(|s| std::cmp::Reverse(s.duration));

        let mut out = String::from("Build step timings (slowest executed steps):\n");
        for step in executed.iter().take(SUMMARY_SLOWEST_STEPS) {
            let duration = step.duration.unwrap_or_default();
            out.push_str(&format!(
                "  {:>8}  {}{}\n",
                format_duration(duration),
                if step.failed { "FAILED " } else { "" },
                truncate(&step.name, SUMMARY_NAME_MAX_CHARS)
            ));
        }
        out.push_str(&format!(
            "Build steps: {} total, {} cached, {} executed; {} wall time\n",
            steps.len(),
            cached,
            steps.len() - cached,
            format_duration(wall)
        ));
        Some(out)
    }
}

fn elapsed(started: VertexTimestamp, completed: VertexTimestamp) -> Duration {
    let nanos =
        |(secs, nanos): VertexTimestamp| i128::from(secs) * 1_000_000_000 + i128::from(nanos);
    let delta = nanos(completed) - nanos(started);
    // A clock step backwards between the two stamps would make this negative.
    u64::try_from(delta).map_or(Duration::ZERO, Duration::from_nanos)
}

fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs_f64();
    if secs >= 60.0 {
        let minutes = (secs / 60.0).floor();
        format!("{}m {:04.1}s", minutes as u64, secs - minutes * 60.0)
    } else {
        format!("{secs:.1}s")
    }
}

fn truncate(name: &str, max_chars: usize) -> String {
    // Collapse the `\` line continuations BuildKit keeps in RUN names, and
    // drop `--mount=...` flags: they are long and say nothing about which
    // command was slow.
    let flat = name
        .split_whitespace()
        .filter(|token| *token != "\\" && !token.starts_with("--mount="))
        .collect::<Vec<_>>()
        .join(" ");
    if flat.chars().count() <= max_chars {
        flat
    } else {
        let cut: String = flat.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(secs: i64, millis: i32) -> Option<VertexTimestamp> {
        Some((secs, millis * 1_000_000))
    }

    #[test]
    fn duration_spans_start_to_completion_across_updates() {
        let mut timer = BuildStepTimer::new();
        timer.observe(
            "sha256:a",
            "[build 5/9] RUN npm ci",
            false,
            ts(100, 0),
            None,
            "",
        );
        timer.observe(
            "sha256:a",
            "[build 5/9] RUN npm ci",
            false,
            ts(100, 0),
            ts(131, 500),
            "",
        );

        let steps = timer.steps();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].duration, Some(Duration::from_millis(31_500)));
        assert!(!steps[0].cached);
    }

    #[test]
    fn incomplete_step_has_no_duration() {
        let mut timer = BuildStepTimer::new();
        timer.observe(
            "sha256:a",
            "[build 1/2] RUN sleep 99",
            false,
            ts(5, 0),
            None,
            "",
        );
        assert_eq!(timer.steps()[0].duration, None);
    }

    #[test]
    fn summary_lists_executed_steps_slowest_first_and_counts_cache_hits() {
        let mut timer = BuildStepTimer::new();
        timer.observe(
            "d1",
            "[build 2/5] COPY package.json .",
            true,
            ts(0, 0),
            ts(0, 1),
            "",
        );
        timer.observe(
            "d2",
            "[build 3/5] RUN npm ci",
            false,
            ts(0, 0),
            ts(20, 0),
            "",
        );
        timer.observe(
            "d3",
            "[build 5/5] RUN npm run build",
            false,
            ts(20, 0),
            ts(95, 300),
            "",
        );

        let summary = timer.summary(Duration::from_secs(101)).unwrap();
        let build_line = summary.find("RUN npm run build").unwrap();
        let install_line = summary.find("RUN npm ci").unwrap();
        assert!(
            build_line < install_line,
            "slowest step must come first:\n{summary}"
        );
        assert!(summary.contains("1m 15.3s"), "{summary}");
        assert!(summary.contains("20.0s"), "{summary}");
        assert!(
            !summary.contains("COPY package.json"),
            "cached steps are not listed:\n{summary}"
        );
        assert!(
            summary.contains("Build steps: 3 total, 1 cached, 2 executed; 1m 41.0s wall time"),
            "{summary}"
        );
    }

    #[test]
    fn failed_step_is_marked() {
        let mut timer = BuildStepTimer::new();
        timer.observe(
            "d1",
            "[build 3/5] RUN npm ci",
            false,
            ts(0, 0),
            ts(4, 0),
            "exit code: 1",
        );
        let summary = timer.summary(Duration::from_secs(5)).unwrap();
        assert!(
            summary.contains("FAILED [build 3/5] RUN npm ci"),
            "{summary}"
        );
    }

    #[test]
    fn no_vertices_means_no_summary() {
        assert_eq!(BuildStepTimer::new().summary(Duration::from_secs(1)), None);
    }

    #[test]
    fn vertices_without_digest_or_name_are_ignored() {
        let mut timer = BuildStepTimer::new();
        timer.observe("", "[build 1/1] RUN true", false, ts(0, 0), ts(1, 0), "");
        timer.observe("d1", "", false, ts(0, 0), ts(1, 0), "");
        assert!(timer.steps().is_empty());
    }

    #[test]
    fn tracked_steps_are_bounded() {
        let mut timer = BuildStepTimer::new();
        for i in 0..(MAX_TRACKED_STEPS + 10) {
            timer.observe(&format!("d{i}"), "step", false, None, None, "");
        }
        assert_eq!(timer.steps().len(), MAX_TRACKED_STEPS);
    }

    #[test]
    fn clock_going_backwards_yields_zero_not_a_panic() {
        assert_eq!(elapsed((10, 0), (9, 0)), Duration::ZERO);
    }

    #[test]
    fn long_multiline_names_are_flattened_and_truncated() {
        let name = format!("[build 7/9] RUN echo \\\n    {}", "x".repeat(300));
        let out = truncate(&name, 40);
        assert_eq!(out.chars().count(), 40);
        assert!(out.starts_with("[build 7/9] RUN echo xxx"));
        assert!(out.ends_with('…'));
    }

    #[test]
    fn mount_flags_are_dropped_from_step_names() {
        let name =
            "[build 5/8] RUN --mount=type=cache,target=/cache/npm,id=npm_store_app,sharing=locked \
                    --mount=type=secret,id=npmrc npm install";
        assert_eq!(truncate(name, 110), "[build 5/8] RUN npm install");
    }
}
