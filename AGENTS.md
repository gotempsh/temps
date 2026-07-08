# AGENTS.md

Conventions for AI coding agents working on this repo (Claude Code,
Codex, aider, etc.). The detailed engineering rules live in
[`CLAUDE.md`](./CLAUDE.md); this file is the short list of process
conventions that go *around* the code. Read both.

## Always update `CHANGELOG.md`

Every user-visible change in this repo lands with a `CHANGELOG.md`
entry under `## [Unreleased]`, in the same commit as the code change.
"User-visible" means anything an operator could notice: behaviour
change, new flag, new endpoint, removed flag, UI change, performance
characteristic, error-message format, dependency bump that changes
the operator surface. Internal refactors with no observable impact
don't need an entry, but when in doubt, write one.

The file follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/):
- Sections: `### Added`, `### Changed`, `### Removed`, `### Fixed`,
  `### Tests` (last is project-specific).
- Each bullet starts with a **bolded short headline**, then a colon,
  then a self-contained explanation. Include *why* — not just *what*.
- Reference migration filenames, endpoint paths, env vars, and crate
  names by their exact identifiers so the entry is greppable later.
- Test-only changes go under `### Tests`.

If you're touching code without writing a CHANGELOG entry, you're
either doing the wrong thing or you forgot. Stop and add the entry
before staging the commit.

**This is CI-enforced on every PR to `main`.** The `changelog-check`
workflow fails the PR unless the diff touches `CHANGELOG.md` (with a
valid `## [Unreleased]` category) **or** the PR carries the
`skip-changelog` label. So for every PR you open, do one of:

- **Add a `CHANGELOG.md` entry** (the default — see above). This is
  also required for changes to the `@temps-sdk/cli` npm package, even
  though it versions separately; tag those bullets with `(\`@temps-sdk/cli\`, #PR)`.
- **Apply the `skip-changelog` label** (`gh pr edit <n> --add-label
  skip-changelog`) only when the change is genuinely changelog-exempt:
  docs/typos, CI/build config, dependency bumps with no operator
  impact, pure refactors, or test-only changes.

Don't open a PR and leave the changelog check red — resolve it the
same way you'd resolve a failing test.

## Use the generated OpenAPI SDK in `web/`

The frontend has a generated TypeScript SDK at `web/src/api/client/`
(`types.gen.ts`, `sdk.gen.ts`, `@tanstack/react-query.gen.ts`) produced
by `bun run openapi-ts` against the running backend. **Use it.**

- Do not write hand-rolled `fetch` helpers under `web/src/lib/`. There
  used to be one (`backup-schedules.ts`) and it caused a real bug —
  someone added a field to the backend, forgot to mirror it in the
  shim's local type, and a UI feature silently dropped the field on
  PATCH.
- If a binding you need is missing from the generated SDK, the cause
  is the backend handler isn't fully decorated for OpenAPI. Fix it
  there: add `#[utoipa::path]`, register the schema in `ApiDoc`,
  restart the server, regenerate. Don't paper over with a `fetch`
  shim.
- If you can't get the binding to generate, **ask for help** before
  reaching for a shim. The shim creates two copies of the API surface
  that drift apart.

## Restart the server when you change the OpenAPI surface

If your backend change touches handlers, request/response shapes,
schemas, or routes, you must:
1. Restart `temps serve` (use the `start-temps` skill).
2. `cd web && bun run openapi-ts` to regenerate the SDK against the
   live server.
3. Commit the regenerated files. They're tracked in git on purpose so
   reviewers see the API delta.

The shortest way to spot a missing step: TypeScript compile errors
in `web/src/` that say "Module ... has no exported member ...". That
means the SDK is stale.

## Scope Docker usage on shared hosts

This host may already be running a live Temps instance or other
operator-owned Docker resources. Do not stop, remove, prune, rebuild,
retag, or otherwise mutate existing containers, images, volumes, or
networks. Docker-backed tests may create uniquely named temporary
resources and must clean up only the resources created by that test run.

## Pre-commit hooks run cargo fmt and cargo clippy

Hooks **will** reformat your files and **will** fail the commit if
clippy finds issues. Plan for it:

- Don't fight the formatter. If `cargo fmt` modifies a file during a
  commit, re-stage and commit again.
- Multiple atomic commits run hooks once each. If you're committing
  three related changes, prefer one commit so clippy/fmt run once.
  (The wall-clock cost of clippy on this workspace is ~3–5 min.)
- Never pass `--no-verify` unless the user explicitly asks. CLAUDE.md
  forbids it. If a hook is broken, fix the hook, don't bypass it.

## Conventional Commits

Already in CLAUDE.md, but reinforced here because it's a hard rule:
`type(scope): description` where type is one of `feat`, `fix`,
`docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`,
`revert`. Scope is the affected crate or area (`backup`, `web`,
`deployments`, etc.).

The `Changelog` CI check validates *every* commit in the PR's
`base..HEAD` range, not just the tip — one bad commit fails the whole
check. `git revert` defaults to `Revert "original message"`, which is
not conventional. Never use `git revert --no-edit` and leave it —
either pass an explicit conventional `-m`, or amend right after.

## Per-record config columns, not env vars

When adding a new runtime knob, default to a column on the relevant
entity table — never a new `TEMPS_*` env var. Examples of the kind of
config this covers: per-OIDC-provider `trust_idp_email`, per-project
feature toggles, per-service quota overrides.

Why:
- Env vars are global and process-scoped. Changing one for *one*
  provider/project/tenant forces a binary restart and accidentally
  changes everyone else's behaviour too.
- DB columns are per-record, mutable at runtime via the API/UI, and
  get audit-logged through the normal handler write path.
- The setting survives binary upgrades and re-installs without
  operators having to re-export shell variables.

If the knob is *truly* installation-wide (e.g. the listen address of
the binary itself), env vars are still fine — but the bar is "this
setting can only have one value per running process, ever". Almost
nothing meets that bar. If you're tempted to add `TEMPS_FOO_BAR=1`,
ask first whether `entity.foo_bar bool` would do the job.

## New features must scale on small resources

Temps runs as a single binary on small machines (reference: 3 vCPU /
4 GB RAM) while the proxy path may see 100k+ req/s. Every new feature
must be designed for that from the start — efficiency is a
requirement, not a follow-up optimization. The full rules live in
[`CLAUDE.md` → Scalability & Efficiency](./CLAUDE.md#scalability--efficiency);
the short version:

- Classify your code: **hot path** (per-request/per-event) vs
  **control plane** (handlers, background jobs).
- Hot path: no locks, no per-operation I/O, no unbounded channels or
  cardinality — aggregate with atomics and flush in batches.
- Everywhere: stream instead of buffering unbounded data, batch DB
  writes, make background loops O(changes) not O(total rows).
- PRs touching the hot path or high-volume data flows must state
  expected load, memory bound, and behaviour at saturation.

## Don't sweep unrelated dirty files into your commits

If you arrive at a working tree that's already dirty (because a
previous session left files modified), confirm with the user whether
to include those files before staging them. Sweeping unrelated work
into a focused PR makes review slower and history harder to bisect.
