// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure pieces of an autofix incident (ADR-054 section 5): reading the agent's
//! report, measuring the diff, matching protected paths, and scoring the fix
//! from evidence the server collected itself.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Fenced block the agent ends its work with.
pub const REPORT_FENCE: &str = "autofix-report";

/// What the agent says it did. Only `reproduction_command` changes what the
/// server does (it is run again by the server); the rest is shown to the
/// reviewer as the agent's account, never as evidence.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct AgentReport {
    #[serde(default)]
    pub reproduced: Option<bool>,
    /// Shell command, run from the repository root, that fails while the bug
    /// is present and passes once it is fixed.
    #[serde(default)]
    pub reproduction_command: Option<String>,
    #[serde(default)]
    pub root_cause: Option<String>,
    #[serde(default)]
    pub fix_summary: Option<String>,
    /// The model's own estimate, 1-5; see [`self_assessment_out_of_five`].
    #[serde(default)]
    pub confidence: Option<f64>,
}

impl AgentReport {
    pub fn reproduction_command(&self) -> Option<&str> {
        self.reproduction_command
            .as_deref()
            .map(str::trim)
            .filter(|command| !command.is_empty())
    }

    /// Root cause and fix as one paragraph for the incident summary.
    pub fn summary(&self) -> Option<String> {
        let parts: Vec<String> = [
            self.root_cause
                .as_deref()
                .map(|text| format!("Root cause: {}", text.trim())),
            self.fix_summary
                .as_deref()
                .map(|text| format!("Fix: {}", text.trim())),
        ]
        .into_iter()
        .flatten()
        .filter(|text| text.len() > 6)
        .collect();
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    }
}

/// The last well-formed ```autofix-report block in `text`, if any.
pub fn parse_report(text: &str) -> Option<AgentReport> {
    let opener = format!("```{REPORT_FENCE}");
    let mut found = None;
    let mut rest = text;
    while let Some(start) = rest.find(&opener) {
        let after = &rest[start + opener.len()..];
        let Some(end) = after.find("```") else {
            break;
        };
        if let Ok(report) = serde_json::from_str::<AgentReport>(after[..end].trim()) {
            found = Some(report);
        }
        rest = &after[end + 3..];
    }
    found
}

/// One changed path, from `git diff --numstat`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FileChange {
    pub path: String,
    /// `None` for binary files.
    pub added: Option<u32>,
    pub removed: Option<u32>,
}

/// Parse `git diff --numstat` output. Renames (`a => b`) keep the new path.
pub fn parse_numstat(output: &str) -> Vec<FileChange> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let added = fields.next()?.trim();
            let removed = fields.next()?.trim();
            let path = fields.next()?.trim();
            if path.is_empty() {
                return None;
            }
            let path = renamed_to(path);
            Some(FileChange {
                path,
                added: added.parse().ok(),
                removed: removed.parse().ok(),
            })
        })
        .collect()
}

/// The new path of a numstat entry: `src/{old => new}.ts` → `src/new.ts`,
/// `old.ts => new.ts` → `new.ts`, anything else unchanged.
fn renamed_to(path: &str) -> String {
    if let (Some(open), Some(close)) = (path.find('{'), path.rfind('}')) {
        if open < close {
            if let Some((_, new)) = path[open + 1..close].split_once(" => ") {
                let joined = format!("{}{}{}", &path[..open], new, &path[close + 1..]);
                return joined.replace("//", "/");
            }
        }
    }
    match path.split_once(" => ") {
        Some((_, new)) => new.to_string(),
        None => path.to_string(),
    }
}

/// Lines added plus removed across `changes`; binary files count as 1.
pub fn changed_lines(changes: &[FileChange]) -> u32 {
    changes
        .iter()
        .map(|change| match (change.added, change.removed) {
            (Some(added), Some(removed)) => added + removed,
            _ => 1,
        })
        .sum()
}

/// Glob match for protected paths: `*` stays within one path segment, `**`
/// spans any number of segments, `?` is one character.
///
/// Both levels use the classic greedy wildcard match with a single backtrack
/// point (the most recent star), which is O(pattern * text) in the worst
/// case. Patterns are operator input and run on the single autofix worker,
/// so a naive recursive matcher -- exponential in the number of stars --
/// would let one pattern hang every incident.
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    /// One path segment: `*` is any run of bytes, `?` exactly one byte.
    fn segment(pattern: &[u8], text: &[u8]) -> bool {
        wildcard_match(
            pattern,
            text,
            |token| *token == b'*',
            |token, byte| *token == b'?' || token == byte,
        )
    }
    /// Whole path: a `**` segment is any run of segments; any other pattern
    /// segment must match exactly one path segment.
    fn segments(pattern: &[&str], path: &[&str]) -> bool {
        wildcard_match(
            pattern,
            path,
            |token| *token == "**",
            |token, part| segment(token.as_bytes(), part.as_bytes()),
        )
    }
    let pattern = pattern.trim().trim_start_matches("./");
    if pattern.is_empty() {
        return false;
    }
    let pattern: Vec<&str> = pattern.split('/').filter(|part| !part.is_empty()).collect();
    let path: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    segments(&pattern, &path)
}

/// Greedy wildcard match of `pattern` against `text`. `is_star` marks a token
/// that matches any run (including none) of text items; `matches_one` says
/// whether a non-star token matches exactly one text item. Only the most
/// recent star is ever backtracked to, so each text position is retried at
/// most once per star: O(pattern.len() * text.len()) `matches_one` calls.
fn wildcard_match<P, T>(
    pattern: &[P],
    text: &[T],
    is_star: impl Fn(&P) -> bool,
    matches_one: impl Fn(&P, &T) -> bool,
) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    // (pattern index of the last star, text index it is currently absorbing up to)
    let mut backtrack: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some(token) if is_star(token) => {
                // Try the star as empty first; remember where to resume.
                backtrack = Some((p, t));
                p += 1;
            }
            Some(token) if matches_one(token, &text[t]) => {
                p += 1;
                t += 1;
            }
            _ => match backtrack {
                // Let the last star absorb one more item and retry after it.
                Some((star, absorbed)) => {
                    backtrack = Some((star, absorbed + 1));
                    p = star + 1;
                    t = absorbed + 1;
                }
                None => return false,
            },
        }
    }
    // Text exhausted: only trailing stars may remain.
    pattern[p..].iter().all(is_star)
}

/// Most wildcard characters (`*`, `?`) one protected path may contain.
pub const MAX_PROTECTED_PATH_WILDCARDS: usize = 32;

/// Why a protected-path glob is refused, or `None` when it is acceptable.
/// `*` and `**` are the only star forms; a longer run of stars has no extra
/// meaning and is rejected rather than silently reinterpreted.
pub fn protected_path_problem(pattern: &str) -> Option<String> {
    let mut run = 0usize;
    let mut longest_run = 0usize;
    let mut wildcards = 0usize;
    for byte in pattern.bytes() {
        if byte == b'*' {
            run += 1;
            longest_run = longest_run.max(run);
        } else {
            run = 0;
        }
        if byte == b'*' || byte == b'?' {
            wildcards += 1;
        }
    }
    if longest_run > 2 {
        return Some(format!(
            "protected path '{pattern}' has {longest_run} consecutive '*': use '*' within a \
             segment or '**' for any number of directories"
        ));
    }
    if wildcards > MAX_PROTECTED_PATH_WILDCARDS {
        return Some(format!(
            "protected path '{pattern}' has {wildcards} wildcards; at most \
             {MAX_PROTECTED_PATH_WILDCARDS} are allowed"
        ));
    }
    None
}

/// Changed paths that match any protected glob.
pub fn protected_matches(patterns: &[String], changes: &[FileChange]) -> Vec<String> {
    changes
        .iter()
        .filter(|change| {
            patterns
                .iter()
                .any(|pattern| glob_matches(pattern, &change.path))
        })
        .map(|change| change.path.clone())
        .collect()
}

/// Whether any changed file is one the stack trace names. Stack paths are
/// often absolute or prefixed (`webpack://app/./src/x.ts`), so a changed path
/// matches when a frame's file ends with it.
pub fn touches_stack(stack_files: &[String], changes: &[FileChange]) -> bool {
    changes.iter().any(|change| {
        let path = change.path.trim_start_matches("./");
        stack_files.iter().any(|file| {
            let file = file.trim_start_matches("./");
            file == path
                || file.ends_with(&format!("/{path}"))
                || path.ends_with(&format!("/{file}"))
        })
    })
}

/// Result of a command the server ran in the sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CommandOutcome {
    pub command: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    /// Last bytes of stdout and stderr, at most 4 KB.
    pub output_excerpt: String,
}

impl CommandOutcome {
    pub fn passed(&self) -> bool {
        self.exit_code == 0
    }
}

/// Everything the score is computed from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Evidence {
    /// The reproduction at the deployed source: `Some(false)` means it
    /// failed, i.e. the bug was reproduced.
    pub reproduction_at_base: Option<CommandOutcome>,
    /// Every run of the reproduction after the fix (up to three).
    pub reproduction_after_fix: Vec<CommandOutcome>,
    /// The project's own test command after the fix, when one is configured.
    pub suite_after_fix: Option<CommandOutcome>,
    pub changes: Vec<FileChange>,
    pub touches_stack: bool,
    pub protected_paths: Vec<String>,
    pub self_assessment: Option<f64>,
}

/// One check behind the score, as stored on the incident and shown to the
/// reviewer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Signal {
    pub key: String,
    pub label: String,
    /// `Some(true)` met, `Some(false)` not met, `None` not collected.
    pub met: Option<bool>,
    /// Level of the confidence ladder this check belongs to, 1-5; `None` for
    /// context that is shown but never scored (the agent's own confidence).
    pub level: Option<i16>,
    pub detail: String,
    /// The command that produced it, when it came from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<CommandOutcome>,
}

/// The score and how it was reached.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Assessment {
    /// Highest level of the confidence ladder the fix reached, 0-5.
    pub score: i16,
    pub signals: Vec<Signal>,
    /// What the next level needs and why it was not reached, below the top.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_level: Option<String>,
    /// Paths that block a policy merge.
    pub protected_paths: Vec<String>,
}

const SMALL_DIFF_LINES: u32 = 50;
const SMALL_DIFF_FILES: usize = 4;

/// Highest score a fix can get.
pub const MAX_SCORE: i16 = 5;

/// The confidence ladder (ADR-054 section 5): what each score proves. A fix
/// reaches a level only when it meets that level's checks and every level
/// below it, so each score is a statement about the evidence, not a sum.
pub const LEVELS: [(i16, &str); 5] = [
    (1, "The fix passes the agent's reproduction"),
    (
        2,
        "The crash reproduces at the deployed source and the fix makes it pass",
    ),
    (
        3,
        "The fix is small and changes a file from the stack trace",
    ),
    (4, "The project's test command passes"),
    (5, "Replaying the crashing request is clean"),
];

/// What a level proves, for 1-5.
pub fn level_description(level: i16) -> Option<&'static str> {
    LEVELS
        .iter()
        .find(|(candidate, _)| *candidate == level)
        .map(|(_, description)| *description)
}

/// The agent's own confidence on the 1-5 scale. Models fall back to 0-10 or
/// 0-100 whatever they are asked, so a larger value is read on those scales.
pub fn self_assessment_out_of_five(value: f64) -> Option<f64> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let scaled = if value <= 5.0 {
        value
    } else if value <= 10.0 {
        value / 2.0
    } else {
        value / 20.0
    };
    Some(scaled.min(5.0))
}

/// Score a fix from evidence (ADR-054 section 5): the highest level of
/// [`LEVELS`] it reached, 0-5. The agent's own confidence is shown but never
/// scored: it is not evidence.
pub fn assess(evidence: &Evidence) -> Assessment {
    let mut signals = Vec::new();

    let after = &evidence.reproduction_after_fix;
    let fixed = (!after.is_empty()).then(|| after.iter().all(CommandOutcome::passed));
    signals.push(Signal {
        key: "passes_after_fix".into(),
        label: "Same reproduction passes after the fix".into(),
        met: fixed,
        level: Some(1),
        detail: match fixed {
            None => "Not run".into(),
            Some(true) => format!("Passed {} of {} runs", after.len(), after.len()),
            Some(false) => format!(
                "Passed {} of {} runs",
                after.iter().filter(|outcome| outcome.passed()).count(),
                after.len()
            ),
        },
        outcome: after.last().cloned(),
    });

    let reproduced = evidence
        .reproduction_at_base
        .as_ref()
        .map(|outcome| !outcome.passed());
    signals.push(Signal {
        key: "reproduced_at_base".into(),
        label: "Reproduction fails at the deployed source".into(),
        met: reproduced,
        level: Some(2),
        detail: match &evidence.reproduction_at_base {
            None => "No reproduction command was reported, so the bug was not reproduced".into(),
            Some(outcome) if !outcome.passed() => {
                format!("Exited {} without the fix", outcome.exit_code)
            }
            Some(_) => "Passed without the fix, so it does not show the bug".into(),
        },
        outcome: evidence.reproduction_at_base.clone(),
    });

    signals.push(Signal {
        key: "touches_stack".into(),
        label: "Fix changes a file from the stack trace".into(),
        met: Some(evidence.touches_stack),
        level: Some(3),
        detail: if evidence.touches_stack {
            "At least one changed file appears in the stack trace".into()
        } else {
            "No changed file appears in the stack trace".into()
        },
        outcome: None,
    });

    let lines = changed_lines(&evidence.changes);
    let files = evidence.changes.len();
    let small = files > 0 && lines <= SMALL_DIFF_LINES && files < SMALL_DIFF_FILES;
    signals.push(Signal {
        key: "small_diff".into(),
        label: "Diff is small".into(),
        met: Some(small),
        level: Some(3),
        detail: format!(
            "{lines} line(s) in {files} file(s); small is up to {SMALL_DIFF_LINES} lines in fewer than {SMALL_DIFF_FILES} files"
        ),
        outcome: None,
    });

    let suite = evidence
        .suite_after_fix
        .as_ref()
        .map(CommandOutcome::passed);
    signals.push(Signal {
        key: "suite_passes".into(),
        label: "Project test command passes".into(),
        met: suite,
        level: Some(4),
        detail: match suite {
            None => "No test command is configured for this project".into(),
            Some(true) => "Passed".into(),
            Some(false) => "Failed".into(),
        },
        outcome: evidence.suite_after_fix.clone(),
    });

    signals.push(Signal {
        key: "replay".into(),
        label: "Replaying the request no longer errors".into(),
        met: None,
        level: Some(5),
        detail: "Replay is not collected yet".into(),
        outcome: None,
    });

    let self_assessment = evidence
        .self_assessment
        .and_then(self_assessment_out_of_five);
    signals.push(Signal {
        key: "self_assessment".into(),
        label: "Agent's own confidence (not scored)".into(),
        met: self_assessment.map(|value| value >= 3.0),
        level: None,
        detail: match self_assessment {
            Some(value) => format!("{} out of {MAX_SCORE}", value.round() as i16),
            None => "Not reported".into(),
        },
        outcome: None,
    });

    // Climb while every check of the next level is met.
    let mut score = 0;
    let mut next_level = None;
    for (level, description) in LEVELS {
        let checks: Vec<&Signal> = signals
            .iter()
            .filter(|signal| signal.level == Some(level))
            .collect();
        match checks.iter().find(|signal| signal.met != Some(true)) {
            None if !checks.is_empty() => score = level,
            unmet => {
                next_level = Some(match unmet {
                    Some(signal) => format!(
                        "To reach {level}/{MAX_SCORE}: {description}. {}: {}",
                        signal.label, signal.detail
                    ),
                    None => format!("To reach {level}/{MAX_SCORE}: {description}"),
                });
                break;
            }
        }
    }

    Assessment {
        score,
        signals,
        next_level,
        protected_paths: evidence.protected_paths.clone(),
    }
}

/// Keep the last `max` bytes of `text`, on a character boundary.
pub fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &text[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(exit_code: i32) -> CommandOutcome {
        CommandOutcome {
            command: "npm test".into(),
            exit_code,
            duration_ms: 10,
            output_excerpt: String::new(),
        }
    }

    fn change(path: &str, added: u32, removed: u32) -> FileChange {
        FileChange {
            path: path.into(),
            added: Some(added),
            removed: Some(removed),
        }
    }

    #[test]
    fn the_last_report_block_wins_and_malformed_ones_are_skipped() {
        let text = "first\n```autofix-report\n{\"reproduced\": false}\n```\n\
                    then\n```autofix-report\n{\"reproduced\": true, \"reproduction_command\": \"node repro.js\", \"confidence\": 70}\n```\n\
                    ```autofix-report\nnot json\n```";
        let report = parse_report(text).expect("a report");
        assert_eq!(report.reproduced, Some(true));
        assert_eq!(report.reproduction_command(), Some("node repro.js"));
        assert_eq!(report.confidence, Some(70.0));
    }

    #[test]
    fn no_report_block_means_no_report() {
        assert!(parse_report("I fixed it, trust me").is_none());
        assert!(parse_report("```autofix-report\n{\"reproduced\": true}").is_none());
    }

    #[test]
    fn a_blank_reproduction_command_is_absent() {
        let report = AgentReport {
            reproduction_command: Some("   ".into()),
            ..Default::default()
        };
        assert_eq!(report.reproduction_command(), None);
    }

    #[test]
    fn numstat_keeps_new_paths_and_binary_files() {
        let changes =
            parse_numstat("3\t1\tsrc/a.ts\n-\t-\tlogo.png\n2\t2\tsrc/{old => new}.ts\n\n");
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0], change("src/a.ts", 3, 1));
        assert_eq!(changes[1].added, None);
        assert_eq!(changes[2].path, "src/new.ts");
        assert_eq!(renamed_to("a.ts => b.ts"), "b.ts");
        assert_eq!(renamed_to("src/{ => lib}/x.ts"), "src/lib/x.ts");
        assert_eq!(changed_lines(&changes), 3 + 1 + 1 + 2 + 2);
    }

    #[test]
    fn globs_respect_segments() {
        assert!(glob_matches("migrations/**", "migrations/2026/001.sql"));
        assert!(glob_matches("**/*.sql", "db/migrations/001.sql"));
        assert!(glob_matches("src/*.ts", "src/app.ts"));
        assert!(!glob_matches("src/*.ts", "src/lib/app.ts"));
        assert!(glob_matches("./.github/**", ".github/workflows/ci.yml"));
        assert!(glob_matches("package?.json", "package1.json"));
        assert!(!glob_matches("", "anything"));
    }

    #[test]
    fn a_single_star_matches_any_run_within_one_segment() {
        assert!(glob_matches("*", "README.md"));
        assert!(glob_matches("*.md", ".md"));
        assert!(glob_matches("a*b*c", "abc"));
        assert!(glob_matches("a*b*c", "axxbyyc"));
        assert!(glob_matches("a*b*c", "abcbc"));
        assert!(!glob_matches("a*b*c", "axxbyy"));
        assert!(!glob_matches("a*b", "a/b"));
        assert!(!glob_matches("*", "src/app.ts"));
        assert!(glob_matches("src/*", "src/app.ts"));
        assert!(!glob_matches("src/*", "src"));
        // Backtracking past an early partial match.
        assert!(glob_matches("*ab", "aab"));
        assert!(glob_matches("*.test.ts", "a.test.test.ts"));
    }

    #[test]
    fn a_double_star_spans_any_number_of_segments() {
        assert!(glob_matches("**", "a/b/c"));
        assert!(glob_matches("**/x", "x"));
        assert!(glob_matches("**/x", "a/b/x"));
        assert!(!glob_matches("**/x", "a/b/x/y"));
        assert!(glob_matches("a/**/b", "a/b"));
        assert!(glob_matches("a/**/b", "a/1/2/b"));
        assert!(glob_matches("a/**/b/**/c", "a/b/x/b/y/c"));
        assert!(!glob_matches("a/**/b", "a/1/2/c"));
        assert!(glob_matches("migrations/**", "migrations"));
        // Inside a segment `**` is just a star: it never crosses `/`.
        assert!(glob_matches("a**b", "axyb"));
        assert!(!glob_matches("a**b", "ax/yb"));
    }

    #[test]
    fn a_question_mark_is_exactly_one_character() {
        assert!(glob_matches("v?.txt", "v1.txt"));
        assert!(!glob_matches("v?.txt", "v.txt"));
        assert!(!glob_matches("v?.txt", "v12.txt"));
        assert!(!glob_matches("a?b", "a/b"));
    }

    /// The recursive matcher this replaced was exponential in the number of
    /// stars: these took effectively forever. The linear matcher answers
    /// them in microseconds; the bound is generous to stay robust on a busy
    /// CI machine.
    #[test]
    fn pathological_patterns_finish_quickly() {
        let started = std::time::Instant::now();
        let name = "a".repeat(40);
        assert!(!glob_matches("********************b", &name));
        assert!(!glob_matches("*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*b", &name));
        let deep = vec!["a"; 40].join("/");
        assert!(!glob_matches(
            "**/a/**/a/**/a/**/a/**/a/**/a/**/a/**/a/**/a/**/a/**/b",
            &deep
        ));
        assert!(glob_matches("*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a", &name));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "glob matching took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn protected_paths_reject_star_runs_and_wildcard_floods() {
        assert_eq!(protected_path_problem("migrations/**"), None);
        assert_eq!(protected_path_problem("**/*.sql"), None);
        assert_eq!(protected_path_problem("package?.json"), None);
        let run = protected_path_problem("********************b").expect("refused");
        assert!(run.contains("20 consecutive '*'"), "{run}");
        assert!(run.contains("********************b"), "{run}");
        assert!(protected_path_problem("a/***/b").is_some());
        let flood = "*a".repeat(MAX_PROTECTED_PATH_WILDCARDS + 1);
        let message = protected_path_problem(&flood).expect("refused");
        assert!(message.contains("at most 32"), "{message}");
        assert_eq!(
            protected_path_problem(&"*a".repeat(MAX_PROTECTED_PATH_WILDCARDS)),
            None
        );
    }

    #[test]
    fn protected_matches_lists_each_hit_once() {
        let changes = vec![change("migrations/001.sql", 1, 0), change("src/a.ts", 1, 0)];
        let hits = protected_matches(&["migrations/**".into(), "**/*.sql".into()], &changes);
        assert_eq!(hits, vec!["migrations/001.sql".to_string()]);
    }

    #[test]
    fn stack_paths_match_by_suffix() {
        let changes = vec![change("app/api/tip/route.ts", 2, 1)];
        assert!(touches_stack(
            &["webpack://app/./app/api/tip/route.ts".into()],
            &changes
        ));
        assert!(touches_stack(
            &["/srv/app/api/tip/route.ts".into()],
            &changes
        ));
        assert!(!touches_stack(&["app/page.tsx".into()], &changes));
    }

    fn signal<'a>(assessment: &'a Assessment, key: &str) -> &'a Signal {
        assessment
            .signals
            .iter()
            .find(|signal| signal.key == key)
            .expect("the signal is always reported")
    }

    fn verified() -> Evidence {
        Evidence {
            reproduction_at_base: Some(outcome(1)),
            reproduction_after_fix: vec![outcome(0), outcome(0), outcome(0)],
            suite_after_fix: Some(outcome(0)),
            changes: vec![change("src/a.ts", 4, 1)],
            touches_stack: true,
            protected_paths: vec![],
            self_assessment: Some(4.0),
        }
    }

    #[test]
    fn a_reproduced_verified_tested_fix_reaches_four() {
        let assessment = assess(&verified());
        // Level 5 needs the replay, which is not collected yet.
        assert_eq!(assessment.score, 4);
        assert_eq!(
            assessment.next_level.as_deref(),
            Some(
                "To reach 5/5: Replaying the crashing request is clean. \
                 Replaying the request no longer errors: Replay is not collected yet"
            )
        );
    }

    #[test]
    fn every_level_needs_the_ones_below_it() {
        // Tests pass but the bug was never reproduced: stuck at 1, not 4.
        let evidence = Evidence {
            reproduction_at_base: Some(outcome(0)),
            ..verified()
        };
        let assessment = assess(&evidence);
        assert_eq!(assessment.score, 1);
        assert!(assessment
            .next_level
            .as_deref()
            .is_some_and(|text| text.starts_with("To reach 2/5")));
    }

    #[test]
    fn a_fix_that_does_not_pass_scores_zero_even_when_flaky() {
        let evidence = Evidence {
            reproduction_after_fix: vec![outcome(0), outcome(1), outcome(0)],
            ..verified()
        };
        let assessment = assess(&evidence);
        assert_eq!(assessment.score, 0);
        let fixed = signal(&assessment, "passes_after_fix");
        assert_eq!(fixed.met, Some(false));
        assert_eq!(fixed.detail, "Passed 2 of 3 runs");
    }

    #[test]
    fn without_a_test_command_a_focused_fix_stops_at_three() {
        let evidence = Evidence {
            suite_after_fix: None,
            ..verified()
        };
        let assessment = assess(&evidence);
        assert_eq!(assessment.score, 3);
        assert!(assessment
            .next_level
            .as_deref()
            .is_some_and(|text| text.contains("No test command is configured")));
    }

    #[test]
    fn a_large_or_off_stack_diff_stops_at_two() {
        let large = Evidence {
            changes: vec![change("src/a.ts", 100, 100)],
            ..verified()
        };
        let assessment = assess(&large);
        assert_eq!(assessment.score, 2);
        assert_eq!(signal(&assessment, "small_diff").met, Some(false));
        let elsewhere = Evidence {
            touches_stack: false,
            ..verified()
        };
        assert_eq!(assess(&elsewhere).score, 2);
    }

    #[test]
    fn nothing_collected_scores_zero() {
        let assessment = assess(&Evidence::default());
        assert_eq!(assessment.score, 0);
        assert!(assessment
            .next_level
            .as_deref()
            .is_some_and(|text| text.starts_with("To reach 1/5")));
    }

    #[test]
    fn every_level_is_described_and_scored_by_a_check() {
        let assessment = assess(&verified());
        for (level, _) in LEVELS {
            assert!(level_description(level).is_some());
            assert!(assessment
                .signals
                .iter()
                .any(|signal| signal.level == Some(level)));
        }
        assert_eq!(LEVELS.len() as i16, MAX_SCORE);
        assert_eq!(level_description(0), None);
    }

    #[test]
    fn the_agents_confidence_is_shown_out_of_five_and_never_scores() {
        assert_eq!(self_assessment_out_of_five(4.0), Some(4.0));
        assert_eq!(self_assessment_out_of_five(8.0), Some(4.0));
        assert_eq!(self_assessment_out_of_five(90.0), Some(4.5));
        assert_eq!(self_assessment_out_of_five(250.0), Some(5.0));
        assert_eq!(self_assessment_out_of_five(-1.0), None);
        assert_eq!(self_assessment_out_of_five(f64::NAN), None);
        let evidence = Evidence {
            self_assessment: Some(95.0),
            ..Default::default()
        };
        let assessment = assess(&evidence);
        let own = signal(&assessment, "self_assessment");
        assert_eq!(own.detail, "5 out of 5");
        assert_eq!(own.level, None);
        assert_eq!(assessment.score, 0);
    }

    #[test]
    fn tail_keeps_the_end_on_a_char_boundary() {
        assert_eq!(tail("short", 10), "short");
        assert_eq!(tail("abcdef", 3), "…def");
        let accented = tail("ééé", 3);
        assert!(accented.ends_with('é'));
    }
}
