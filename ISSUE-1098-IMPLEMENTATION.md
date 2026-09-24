# Issue #1098: Fail invalid static deployment waits promptly

Issue: https://github.com/gotempsh/temps/issues/1098
PR: https://github.com/gotempsh/temps/pull/1114
Branch: `fix/1098-static-deploy-wait`

## Problem

`temps deploy static --wait` could keep polling after a permanent deployment-status error and end with a timeout instead of the HTTP failure. At the investigated base revision, the CLI already used the project-scoped deployment URL, so the remaining defect was error handling in the shared watcher. Optional jobs fetches could also obscure a terminal deployment status.

## Implemented behavior

- A permanent 4xx deployment-status response ends the wait immediately with the HTTP status. Temporary 408, 425, and 429 responses remain retryable within the overall deadline.
- Once a terminal deployment state is known, a failed jobs lookup cannot turn it back into a pending state. The watcher displays the actual failed job message when available, including the backend `failure` field.
- HTTP-backed watcher regression tests exercise permanent errors, transient recovery, failed jobs, and terminal-state handling.

## Verification

- `bun test apps/temps-cli/src/lib/deployment-watcher.test.tsx apps/temps-cli/src/commands/deploy/deploy-static.test.ts`: 21 passed, 0 failed, 67 assertions.
- `bun run typecheck` in `apps/temps-cli`: passed. Normal commit hooks passed; commits are DCO signed.
- `/start-temps` isolated slot 2: backend `/healthz` and web root both returned HTTP 200. This is a startup smoke check; deployment behavior is covered by the watcher tests.

## Operational boundary

The exact end-to-end CLI upload against a deliberately failing live static deployment was not run. PR CI and review remain merge gates.
