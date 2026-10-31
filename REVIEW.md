# Landing PRs: What Reviewers Catch

Distilled from 800 inline review comments on 131 merged and open PRs
(6-23 October 2026). 294 were rated P1 (blocks merge) and 314 P2; almost all came
from the automated reviewer, the rest from maintainers. The findings repeat, so
this file lists the repeats. Before you open a PR, check your change against the
sections that apply and ask "why this and not the alternative?".

Process rules (attribution, commits, DCO) are in [`AGENTS.md`](./AGENTS.md).
Engineering rules (errors, architecture, tests) are in [`AGENTS.md`](./AGENTS.md).
This file covers what those two do not: the failure modes that have blocked merges.

## Every behavior change ships its own test

- Add the test in the same PR. "Verified manually" does not count, even for a one-line fix.
- A fix needs a test that fails without it. Delete the fix and confirm the test fails
  for the reason you expect.
- Several P1 findings were tests the PR itself broke ("Enrollment test now fails",
  "Existing OIDC test now fails", "Sandbox test always fails"). When you change a
  default, message or response shape, grep the test suite for the old behavior.
- Run the tests with Docker available when you change a migration or a column
  default. A new `DEFAULT now()` broke three retention tests that only run against a
  real database ("Retention tests fail with Docker").
- Do not weaken, skip or delete an existing test without a stated reason.

## Authorization and scope

The most expensive findings are access-control gaps.

- **Scope every read by the project (or tenant) the caller was authorized for.** A
  handler that checks access to `project_id` but loads the row by `deployment_id`
  alone lets a caller pass their own project with someone else's deployment
  ("Other projects' capture results leak").
- **Check permissions for every role and path.** Find the sibling handlers, the
  MCP/AI-chat tool versions and background jobs that reach the same data
  ("Project viewers can deploy", "Removed members retain database access",
  "New members miss permission checks", "Admin gate still permits MCP").
- **Do not let a key or token keep working after it is unlinked or removed**
  ("Unlinked keys can keep working", "Worker keys survive unlinking").
- **Cookies and callback hosts are trust boundaries.** Check sibling-app cookies and
  preview-shaped hostnames ("Sibling apps can plant cookies").
- **Every `match` that maps an error to a response lists its variants explicitly.**
  No `_ =>` catch-all (see AGENTS.md). Adding a variant must force a decision about
  its HTTP status.

## Secrets must not reach logs, errors or the UI

- Redact before logging or formatting an error. A secret split across two log
  lines or two error fragments is still leaked ("Split secrets reach logs",
  "Error messages reveal secret fragments", "Env-file logs expose tokens").
- Health paths, image references, host directory names, restore details and CLI
  output are all places secrets have appeared. Check every new string you print.
- Write-only keys must not reveal spend, and "hidden" password fields must not be
  printed by the CLI.

## State that is written late or out of order

- **An older result must not overwrite a newer one.** Async checks, retries,
  recoveries and edits finish out of order ("Old checks overwrite new starts",
  "Older requests restore higher limits", "Saving discards newer edits"). Use a
  generation, version or compare-and-set on the row, and ignore stale completions.
- **Two callers can both pass the check.** For claims on a hostname, bucket, port
  or route number, enforce uniqueness in the database, not in a read-then-write
  ("Two hostname claims can succeed", "Clusters choose the same ports",
  "Projects can share a bucket").
- **Do not publish a value before it is committed**, and do not lose it on
  rollback ("Retry publishes an unsaved number", "Committed route numbers get lost").
- **Do not hold a lock or a pooled database connection across a slow call.**
  Network, Docker and file I/O under a lock caused several findings
  ("Domain guards exhaust database connections", "Database stalls delay route
  notifications", "Cluster actions exhaust database connections").

## Cancellation, failure and cleanup

Every long-running operation (restore, import, build, capture, deploy, drain) needs
answers to these questions in the PR:

1. **What happens on cancel, on timeout and on a server restart halfway through?**
   Cancelled restores, imports and downloads that kept running, or left containers,
   volumes or reserved names behind, were the largest single group of findings.
2. **Does the failure path reset the state?** A failed resend, drain, recovery or
   enrollment must not leave a row stuck in `draining`, `stopped` or `pending`
   ("Failed drain stays draining", "Failed resends get stuck").
3. **Does cleanup remove only what this operation created?** Cleanup that removed
   another service's container, another restore's data or an active network
   ("Deleting removes another service's container", "Cleanup can remove active
   networks").
4. **Is the timeout applied to the whole operation, including setup and queue time?**
   ("Scan setup escapes the deadline", "Queued builds still time out").
5. **Is the process, container or browser killed when the deadline fires?**
   ("Chrome survives the capture timeout", "Timed-out scans keep running").
6. **A transient error must not mark healthy state as failed.** Docker outages
   stopped healthy sandboxes, and rejected calls consumed budgets.

## Resource limits

- Bound every read of a file, an agent event, guest output or an image. Check the size
  before reading it into memory ("Large files bypass the limit", "Guest output
  exhausts server memory", "Bounded reads exceed memory limits").
- Bound parallelism for anything memory-heavy, such as screenshots, PDF work and
  workspace copies.
- A background loop must not open an unbounded number of database connections.

## Migrations and the database

- An ordinary `CREATE INDEX` on a large or hot table blocks writes while it scans.
  Use a nonblocking strategy (for example post-migration maintenance) instead of
  building inside the migration transaction ("Index builds block ingestion").
- Do not backfill every row while `ALTER TABLE` still holds its exclusive lock. Add
  the column first, then backfill in committed batches ("Backfill blocks address
  lookups").
- Verify that the migration works against an existing database, not only an empty one
  ("Error-history index build fails", "Older S3 upgrades fail").

## Frontend (web/)

- **Unsaved edits survive refresh, tab switches and refetches.** Do not reset form
  state when a query refetches ("Refresh resets unsaved variable choices",
  "Switching tabs drops search").
- **Clear dependent selections when their parent changes.** Changing the connection
  must clear the selected repository and branch ("Save uses another connection",
  "Edits carry into another project").
- **Never save something the user did not choose**, such as defaults from browsing or
  unfinished edits ("Browsing saves the repository", "Retry saves unfinished edits").
- **A failed request must not silently drop state**, for example filters
  ("Failed requests silently drop filters").
- Run the web lint and type-check before pushing. Unused imports have broken the
  lint job more than once.

## CI and workflows

- A new test job must provably start. Check that the filter matches at least one test
  ("Runtime tests never start").
- Release and alias scripts must fail closed. A lookup error must not replace
  published tags ("Lookup errors replace published tags").
- Cache steps in `pull_request` workflows need the save guard
  (`.github/scripts/check-cache-save-guards.py`).

## Before you open the PR

- [ ] A test fails without your change and passes with it.
- [ ] You searched for sibling code paths with the same pattern and fixed or listed them
      (sync and async twins, handler and MCP tool, create and update).
- [ ] Authorization is checked for every new read and write, scoped to the caller's project.
- [ ] Cancel, timeout, restart and failure paths are handled and cleaned up.
- [ ] No stored or reusable secret can reach a log line, error message or API response.
      The one exception is a credential minted by the request itself, returned once
      in plaintext with `Cache-Control: no-store` (see AGENTS.md).
- [ ] Reads and parallelism are bounded.
- [ ] `cargo check`, clippy and the touched crates' tests pass, with no new warnings.
