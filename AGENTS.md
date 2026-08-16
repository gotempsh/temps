# AGENTS.md

Conventions for AI coding agents working on this repo (Claude Code,
Codex, aider, etc.). The detailed engineering rules live in
[`CLAUDE.md`](./CLAUDE.md); this file is the short list of process
conventions that go *around* the code. Read both.

## Do not hand-edit `CHANGELOG.md`

`CHANGELOG.md` is generated from Conventional Commits by
[git-cliff](https://git-cliff.org) at release time. PRs must not edit it
directly, because concurrent `[Unreleased]` edits caused constant merge
conflicts.

The `Changelog` workflow validates every non-merge commit in a PR and
posts a preview of the generated entry. A non-conventional commit is
dropped from the changelog, so use a precise `type(scope): description`
subject and make the user or operator impact clear there.

Preview the generated entry locally with:

```bash
scripts/changelog.sh --unreleased
```

The release process regenerates `CHANGELOG.md`; the commit history is
the source of truth.

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

## Never overwrite `apps/temps-cli/openapi.json` with the raw server response

The CLI's SDK is generated from a **committed** copy of the spec at
`apps/temps-cli/openapi.json`. That file is ~92,000 lines of formatted
JSON; the server serves the same document minified on one line, with
keys in whatever order serde produced.

So `curl .../openapi.json > apps/temps-cli/openapi.json` turns a
92,000-line file into a 1-line file, and the pull request reports
**-92,000 deletions** — burying the actual change and making the diff
unreviewable. Pretty-printing alone is not enough either: key order is
not stable between builds, so an unsorted dump reorders huge blocks for
no reason.

Use the script, which fetches, sorts keys recursively, indents by two
and keeps the trailing newline:

```bash
cd apps/temps-cli
TEMPS_API_KEY=tk_... bun run spec:update --url http://localhost:8080/api/api-docs/openapi.json
bun run generate:api        # regenerate the client from the file
bun run scripts/generate-docs.ts --output docs/CLI.md
bun run scripts/generate-docs.ts --format mdx --output docs/CLI.mdx
```

Sanity check before committing — a few new endpoints should be a few
hundred changed lines, never tens of thousands:

```bash
git diff --numstat -- apps/temps-cli/openapi.json
```

You do not have to remember any of this. `bun run spec:check` verifies
the committed file and runs automatically as a pre-commit hook and as the
**OpenAPI Spec Format** CI job, so a minified or reordered spec fails
before review rather than after. It reads only the file on disk — no
server, no network, no `bun install`.

If it fails and the API did *not* change, `bun run spec:check --fix`
reformats in place. If the API *did* change, `bun run spec:update` is
what you want, since `--fix` never fetches.

`web/src/api/client/` has no committed spec; it is generated straight
from the live server by `bun run openapi-ts` (see above), so it does not
have this failure mode.

## Resolving merge conflicts in generated clients

Conflicts in `web/src/api/client/**`, `apps/temps-cli/src/api/**` or
`apps/temps-cli/openapi.json` are conflicts in **build output**. Do not
hand-merge them, and do not hand-pick hunks — the result is a client
that matches neither side's spec.

Take either side to clear the conflict, then regenerate from a server
built off the merged source:

```bash
git checkout --ours -- web/src/api/client apps/temps-cli/src/api apps/temps-cli/openapi.json
git add web/src/api/client apps/temps-cli/src/api apps/temps-cli/openapi.json
# build + start the merged server, then:
cd apps/temps-cli && bun run spec:update --url <server>/api/api-docs/openapi.json && bun run generate:api
cd ../../web && bun run openapi-ts
```

Then `bun run typecheck` (or `npx tsc --noEmit`) in both `web/` and
`apps/temps-cli/`. A clean typecheck is what proves the regenerated
client still satisfies every caller on both sides of the merge.

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

## Features must be discoverable, and unconfigured features must onboard

A feature the user can't find is a feature that doesn't exist. Never
ship a capability whose only entry point is a keyboard shortcut, a
buried menu item, or knowledge the user is assumed to already have.
Every new feature needs a visible surface in the UI where the user is
already looking when they'd want it.

**Optional dependencies do not justify hiding a feature.** When a
feature needs configuration the operator may not have done yet — an AI
provider, an S3 bucket, an SMTP server, a DNS token — the wrong move is
to conditionally render nothing. A self-hosted user has no support
channel: if the button isn't there, they will never learn the feature
exists, and they'll conclude temps can't do it.

Instead, always render the surface and switch it into an onboarding
state:

- **Show what it would do.** Name the capability and give a concrete
  example of the outcome, not an abstract description.
- **Say exactly what's missing.** "No AI provider is configured" — not
  "unavailable" or a disabled control with no explanation.
- **Link straight to the fix.** A direct link to the settings page that
  configures it, deep-linked to the right section. Not "see the docs."
- **Never silently no-op.** If the user triggers it anyway, explain the
  gap; don't fail quietly or spin forever.

Concretely, the shape to reach for:

```tsx
// BAD — the feature vanishes; the user never learns it exists
{aiConfigured && <AiQueryBar />}

// GOOD — always visible, onboards when unconfigured
<AiQueryBar
  configured={aiConfigured}
  onboardingHref="/settings/ai"
  example="show me the users created last week"
/>
```

This applies to the API too: prefer a capability/status endpoint that
reports `configured: false` with a reason and a setup URL over a 404
that leaves the client unable to distinguish "not built" from
"not set up".

## Don't sweep unrelated dirty files into your commits

If you arrive at a working tree that's already dirty (because a
previous session left files modified), confirm with the user whether
to include those files before staging them. Sweeping unrelated work
into a focused PR makes review slower and history harder to bisect.

## Never commit secrets, including local dev-instance artifacts

Never commit `.env` files, credentials, or secrets. This explicitly
includes local dev-instance artifacts generated while running a local
server for manual testing/verification — encryption keys, auth
secrets, generated tokens, `temps_data`-style data directories. These
are easy to sweep in by accident with a broad `git add` right after
spinning up a local test instance to verify a change, which is exactly
when review attention is focused elsewhere. Before staging, run `git
status` and scrutinize every path outside the files you intentionally
edited. If a secret does get committed, treat it as compromised: at
minimum remove it from tracking going forward, and flag to the user
whether history needs rewriting — don't force-push without asking.
