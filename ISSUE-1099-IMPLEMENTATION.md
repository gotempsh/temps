# Issue #1099: Make environment-variable preview scope reliable

Issue: https://github.com/gotempsh/temps/issues/1099
PR: https://github.com/gotempsh/temps/pull/1116
Branch: `fix/1099-explicit-env-scope`

## Problem

A variable linked to production could be marked `include_in_preview` but still be omitted from future preview deployments. The UI and CLI did not make the resulting scope clear before saving, so preview containers could miss required variables.

## Implemented behavior

- The deployment resolver honors explicit preview opt-in for a variable linked to another environment. A preview-specific variable still takes precedence on a key collision. Preview opt-out remains effective.
- New CLI variables exclude unselected future previews by default. `--preview` opts in, while updates preserve the existing preview scope unless explicitly changed.
- The web form shows selected current environments and future-preview behavior before saving.

## Verification

- `cargo check --lib -p temps-deployments`: passed.
- `cargo test --lib -p temps-deployments`: 938 passed, 3 pre-existing ignored. Resolver tests cover linked opt-in, opt-out, and preview override.
- `bun test src/commands/environments/index.test.ts`: 24 passed, including CLI parsing and update-scope preservation.
- CLI and web TypeScript checks passed. Normal pre-commit hooks, including formatting and clippy, passed; commit is DCO signed.

## Operational boundary

A live preview deployment with project-specific secret values was not run. The resolver and CLI tests cover the reported scope behavior. PR CI and review remain merge gates.
