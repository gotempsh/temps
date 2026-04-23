//! `.npmrc` generation from build environment variables.
//!
//! Mirrors the Vercel behavior documented at
//! <https://vercel.com/guides/using-private-dependencies-with-vercel>:
//!
//! * If `NPM_RC` is present, its value is written verbatim (with CRLF
//!   normalization and a guaranteed trailing newline).
//! * Else if `NPM_TOKEN` is present, a minimal `.npmrc` authenticating against
//!   the default public registry is synthesized.
//! * If both are present, `NPM_RC` wins.
//! * If neither is present, nothing is written.
//!
//! The pure planning logic lives here so it is exhaustively unit-testable
//! without touching the filesystem. The `BuildImageJob` calls `plan_npmrc`
//! with the user's build args/env map and writes the resulting file into the
//! build context directory before invoking Docker.

use std::collections::HashMap;

pub(crate) const NPM_RC: &str = "NPM_RC";
pub(crate) const NPM_TOKEN: &str = "NPM_TOKEN";

/// Which env var the `.npmrc` contents were derived from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NpmrcSource {
    /// User provided a complete `.npmrc` via `NPM_RC`.
    FromNpmRc,
    /// Synthesized from a bare `NPM_TOKEN` for the default registry.
    FromNpmToken,
}

impl NpmrcSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            NpmrcSource::FromNpmRc => "NPM_RC",
            NpmrcSource::FromNpmToken => "NPM_TOKEN",
        }
    }
}

/// A plan to write `.npmrc`. `None` means no `.npmrc` should be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NpmrcPlan {
    pub contents: String,
    pub source: NpmrcSource,
}

/// Build a `.npmrc` plan from a map of env vars / build args.
///
/// Env var names are matched case-sensitively (they are env vars, after all).
/// Returns `None` when neither `NPM_RC` nor `NPM_TOKEN` is present, or when
/// both are present but empty.
pub(crate) fn plan_npmrc(env: &HashMap<String, String>) -> Option<NpmrcPlan> {
    if let Some(raw) = env.get(NPM_RC) {
        if !raw.is_empty() {
            return Some(NpmrcPlan {
                contents: normalize(raw),
                source: NpmrcSource::FromNpmRc,
            });
        }
    }

    if let Some(token) = env.get(NPM_TOKEN) {
        if !token.is_empty() {
            let contents = format!(
                "registry=https://registry.npmjs.org/\n//registry.npmjs.org/:_authToken={}\n",
                token
            );
            return Some(NpmrcPlan {
                contents,
                source: NpmrcSource::FromNpmToken,
            });
        }
    }

    None
}

/// Normalize `.npmrc` contents pasted via env var: convert CRLF to LF and
/// ensure a trailing newline so appends/concat are safe.
fn normalize(raw: &str) -> String {
    let mut out = raw.replace("\r\n", "\n").replace('\r', "\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn returns_none_when_neither_is_set() {
        let e = env(&[("OTHER", "value")]);
        assert_eq!(plan_npmrc(&e), None);
    }

    #[test]
    fn uses_npm_rc_verbatim_when_only_npm_rc_is_set() {
        let raw =
            "@scope:registry=https://npm.pkg.github.com/\n//npm.pkg.github.com/:_authToken=TOK\n";
        let e = env(&[("NPM_RC", raw)]);
        let plan = plan_npmrc(&e).expect("plan");
        assert_eq!(plan.source, NpmrcSource::FromNpmRc);
        assert_eq!(plan.contents, raw);
    }

    #[test]
    fn synthesizes_from_npm_token_when_only_npm_token_is_set() {
        let e = env(&[("NPM_TOKEN", "abc123")]);
        let plan = plan_npmrc(&e).expect("plan");
        assert_eq!(plan.source, NpmrcSource::FromNpmToken);
        assert_eq!(
            plan.contents,
            "registry=https://registry.npmjs.org/\n//registry.npmjs.org/:_authToken=abc123\n"
        );
    }

    #[test]
    fn npm_rc_takes_precedence_over_npm_token_when_both_are_set() {
        let rc = "registry=https://custom.example.com/\n";
        let e = env(&[("NPM_RC", rc), ("NPM_TOKEN", "ignored")]);
        let plan = plan_npmrc(&e).expect("plan");
        assert_eq!(plan.source, NpmrcSource::FromNpmRc);
        assert_eq!(plan.contents, rc);
    }

    #[test]
    fn normalizes_crlf_line_endings() {
        let raw = "registry=https://a.example.com/\r\n//a.example.com/:_authToken=T\r\n";
        let e = env(&[("NPM_RC", raw)]);
        let plan = plan_npmrc(&e).expect("plan");
        assert!(!plan.contents.contains('\r'));
        assert_eq!(
            plan.contents,
            "registry=https://a.example.com/\n//a.example.com/:_authToken=T\n"
        );
    }

    #[test]
    fn appends_trailing_newline_when_missing() {
        let raw = "registry=https://a.example.com/";
        let e = env(&[("NPM_RC", raw)]);
        let plan = plan_npmrc(&e).expect("plan");
        assert!(plan.contents.ends_with('\n'));
    }

    #[test]
    fn empty_values_are_treated_as_absent() {
        let e = env(&[("NPM_RC", ""), ("NPM_TOKEN", "")]);
        assert_eq!(plan_npmrc(&e), None);

        // Empty NPM_RC but valid NPM_TOKEN → fall through to synthesis.
        let e = env(&[("NPM_RC", ""), ("NPM_TOKEN", "t")]);
        let plan = plan_npmrc(&e).expect("plan");
        assert_eq!(plan.source, NpmrcSource::FromNpmToken);
    }

    #[test]
    fn env_keys_are_case_sensitive() {
        // Lowercase/alternate-case keys must not trigger the feature.
        let e = env(&[("npm_rc", "x"), ("Npm_Token", "y")]);
        assert_eq!(plan_npmrc(&e), None);
    }

    #[test]
    fn plan_contents_can_be_written_to_disk() {
        // Smoke test mirroring what BuildImageJob::ensure_npmrc does:
        // plan_npmrc + fs::write into a build context directory.
        let dir = tempfile::tempdir().expect("tempdir");
        let e = env(&[("NPM_TOKEN", "xyz789")]);
        let plan = plan_npmrc(&e).expect("plan");

        let path = dir.path().join(".npmrc");
        std::fs::write(&path, &plan.contents).expect("write");

        let on_disk = std::fs::read_to_string(&path).expect("read");
        assert_eq!(on_disk, plan.contents);
        assert!(on_disk.contains("xyz789"));
        assert!(on_disk.contains("registry=https://registry.npmjs.org/"));
    }
}
