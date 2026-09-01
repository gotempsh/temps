// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

pub struct PromptBuilder;

impl PromptBuilder {
    /// Build a prompt instructing the AI CLI to locate and fix the error in the repository.
    pub fn build_error_fix_prompt(
        project_name: &str,
        error_type: &str,
        error_message: &str,
        stack_trace: &str,
        total_count: i32,
        first_seen: &str,
        environment_name: Option<&str>,
    ) -> String {
        let env_line = match environment_name {
            Some(env) => format!("Environment: {}\n", env),
            None => String::new(),
        };

        format!(
            r#"You are an automated software engineer fixing a bug in the project "{project_name}".

## Error Details

Error type: {error_type}
Error message: {error_message}
{env_line}Occurrences: {total_count}
First seen: {first_seen}

## Stack Trace

```
{stack_trace}
```

## Instructions

1. Analyse the stack trace and understand the root cause of this error.
2. Find the relevant source file(s) in the repository.
3. Apply the minimal fix required to resolve the error without breaking existing behaviour.
4. Run the project's test suite (if any) to verify the fix does not introduce regressions:
   - Look for `package.json`, `Makefile`, `Cargo.toml`, `pyproject.toml`, or similar to identify test commands.
   - Run tests using the appropriate command (e.g. `npm test`, `cargo test`, `pytest`, etc.).
5. Commit your changes with a message of the form: `fix: <concise description of the error fixed>`
6. Do NOT modify files unrelated to this bug fix.
7. Do NOT change configuration files, lock files, or CI/CD pipelines unless they are the direct cause of the error.

Focus on correctness and minimal diff. Prefer targeted surgical fixes over large refactors.
"#,
            project_name = project_name,
            error_type = error_type,
            error_message = error_message,
            env_line = env_line,
            total_count = total_count,
            first_seen = first_seen,
            stack_trace = stack_trace,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_error_fix_prompt_contains_key_fields() {
        let prompt = PromptBuilder::build_error_fix_prompt(
            "my-app",
            "ReferenceError",
            "foo is not defined",
            "at handler.js:42",
            15,
            "2026-01-01T00:00:00Z",
            Some("production"),
        );

        assert!(prompt.contains("my-app"));
        assert!(prompt.contains("ReferenceError"));
        assert!(prompt.contains("foo is not defined"));
        assert!(prompt.contains("at handler.js:42"));
        assert!(prompt.contains("15"));
        assert!(prompt.contains("2026-01-01T00:00:00Z"));
        assert!(prompt.contains("production"));
        assert!(prompt.contains("fix: "));
    }

    #[test]
    fn test_build_error_fix_prompt_without_environment() {
        let prompt = PromptBuilder::build_error_fix_prompt(
            "my-app",
            "TypeError",
            "Cannot read property",
            "at app.js:10",
            1,
            "2026-01-01T00:00:00Z",
            None,
        );

        assert!(prompt.contains("my-app"));
        assert!(!prompt.contains("Environment:"));
    }

    #[test]
    fn test_prompt_contains_error_details() {
        let prompt = PromptBuilder::build_error_fix_prompt(
            "my-app",
            "TypeError",
            "Cannot read property 'map' of undefined",
            "  at UserList.render (src/components/UserList.tsx:42:18)",
            47,
            "2026-03-30T07:00:00Z",
            Some("production"),
        );
        assert!(prompt.contains("TypeError"));
        assert!(prompt.contains("Cannot read property 'map' of undefined"));
        assert!(prompt.contains("UserList.tsx:42"));
        assert!(prompt.contains("production"));
        assert!(prompt.contains("my-app"));
        assert!(prompt.contains("47"));
        assert!(prompt.contains("2026-03-30T07:00:00Z"));
    }

    #[test]
    fn test_prompt_handles_empty_stack_trace() {
        let prompt = PromptBuilder::build_error_fix_prompt(
            "app",
            "Error",
            "Something failed",
            "",
            1,
            "2026-01-01T00:00:00Z",
            None,
        );
        assert!(prompt.contains("Error"));
        assert!(prompt.contains("Something failed"));
        // The prompt still includes the Stack Trace section header even when the trace is empty
        // — the empty code block ```` ``` ``` ```` will be present. We verify no stack content slipped in.
        assert!(!prompt.contains("at "));
    }

    #[test]
    fn test_prompt_contains_instructions() {
        let prompt =
            PromptBuilder::build_error_fix_prompt("app", "Error", "msg", "trace", 1, "now", None);
        // The prompt must include fix instruction and commit format guidance.
        // "fix" appears in both the instructions text and the commit message example.
        // "Commit" (capital C) appears in the step instructing how to commit.
        assert!(prompt.contains("fix"));
        assert!(prompt.contains("Commit") || prompt.contains("commit"));
    }

    #[test]
    fn test_prompt_project_name_is_prominent() {
        let prompt = PromptBuilder::build_error_fix_prompt(
            "my-unique-project-xyz",
            "Error",
            "bad things",
            "trace",
            1,
            "2026-01-01T00:00:00Z",
            None,
        );
        // Project name must appear in the prompt so the AI knows the context
        assert!(
            prompt.contains("my-unique-project-xyz"),
            "project name missing from prompt"
        );
    }

    #[test]
    fn test_prompt_with_environment_includes_env_line() {
        let prompt = PromptBuilder::build_error_fix_prompt(
            "app",
            "Error",
            "msg",
            "trace",
            1,
            "2026-01-01T00:00:00Z",
            Some("staging"),
        );
        assert!(prompt.contains("staging"));
        assert!(prompt.contains("Environment:"));
    }

    #[test]
    fn test_prompt_without_environment_omits_env_line() {
        let prompt = PromptBuilder::build_error_fix_prompt(
            "app",
            "Error",
            "msg",
            "trace",
            1,
            "2026-01-01T00:00:00Z",
            None,
        );
        assert!(!prompt.contains("Environment:"));
    }

    #[test]
    fn test_prompt_occurrence_count_zero() {
        let prompt = PromptBuilder::build_error_fix_prompt(
            "app",
            "NullPointerException",
            "null reference",
            "at Main.main:10",
            0,
            "2026-01-01T00:00:00Z",
            None,
        );
        // 0 occurrences is valid — should still build a prompt
        assert!(prompt.contains("NullPointerException"));
        assert!(prompt.contains("0"));
    }

    #[test]
    fn test_prompt_stack_trace_is_included_in_code_block() {
        let trace = "  at foo (bar.ts:1:2)\n  at baz (qux.ts:3:4)";
        let prompt = PromptBuilder::build_error_fix_prompt(
            "app",
            "Error",
            "oops",
            trace,
            5,
            "2026-01-01T00:00:00Z",
            None,
        );
        // The trace lines must appear verbatim in the prompt
        assert!(prompt.contains("bar.ts:1:2"));
        assert!(prompt.contains("qux.ts:3:4"));
    }
}
