# ADR-033: Release identity invariant for error-tracking symbolication

**Status:** Proposed
**Date:** 2026-07-22
**Author:** David Viejo

> **Numbering note:** Committed ADRs end at 029. Numbers 030–032 are occupied by
> in-flight proposals in separate worktrees (including `adr/032-credential-broker`).
> 033 is the next free number; reviewers may renumber on merge if any of those land
> first.

## Context

### The symbolication join

`temps-error-tracking` symbolicates stack frames — translating minified JS
coordinates back to original source, and attaching source-context lines to native
(Go/Rust/etc.) frames — by joining an inbound error event against stored artifacts
at query time. Both the JavaScript path (`source_maps` table, queried in
`SourceMapService::resolve_frame`) and the native source-context path
(`source_files` table, queried in `SourceMapService::resolve_native_frame`) use
the identical three-column key:

```
(project_id, release, file_path)
```

Both execute at ingest time (inside `symbolicate_error_event`) and again lazily at
read time (inside `symbolicate_stored_event`). In both cases `release` is taken
verbatim from the error event's `release_version` field, which the Sentry SDK
stamps on every event it sends.

If the `release` value in the stored artifact row does not byte-for-byte match the
`release_version` in the event, the SQL filter returns no rows, resolution returns
`None` for every frame, and the developer sees raw minified/compiled frames with no
source context. There is no error message surfaced — the join silently fails.

### Three independently produced values, one invariant

Symbolication requires that three values, produced by three different systems at
three different times, agree on a single immutable string:

1. **SDK-reported release** — the `release` field the application SDK stamps on
   every error event. For interpreted runtimes (Node.js, Python) this comes from
   the `SENTRY_RELEASE` environment variable if set; it can also be baked into the
   SDK init call (`Sentry.init({ release: "…" })` in JS, or
   `ClientOptions { release: Some("…".into()), … }` in the Go/Rust SDKs).

2. **Source-artifact key** — the `release` column value under which source maps
   (JS) and source files (Go/Rust/native) are stored. For JS projects using a
   Sentry-compatible build plugin, this is whatever `SENTRY_RELEASE` was set to at
   build time. For native projects uploading via the Temps CLI/API it is the
   `--release` flag. The `CaptureSourceMapsJob` (and the native
   `CaptureSourceFilesJob`) use `deployment.commit_sha` as this key.

3. **Platform-injected release** — what Temps writes into the running container's
   environment as `SENTRY_RELEASE` (and `OTEL_SERVICE_VERSION`) at deploy time.
   This is the pivot between the other two: if the SDK reads `SENTRY_RELEASE` from
   its environment, and the upload tool reads the same variable at build time, all
   three values are structurally forced to agree.

### The precedence / resolution chain

Both deploy paths use `HashMap::entry(…).or_insert_with(…)` semantics, so a
user-provided `SENTRY_RELEASE` value in project environment settings always wins
and is never overwritten by the platform.

**Git-based deployments** (`plan_git_deployment`) inject
`deployment.commit_sha` — the same string used by `CaptureSourceMapsJob` /
`CaptureSourceFilesJob` as the artifact key, and the same string the Sentry build
plugin reads from `SENTRY_RELEASE` during the Docker build step.

**Image-based deployments** (`plan_docker_image_deployment`) inject a value derived
from the deployed image reference via `image_ref_release`:

- Digest form (`image@sha256:abc123`) → the digest (`sha256:abc123`).
- Tag form (`registry.io/app:v1.2.3`, `registry:5000/app:v1`) → the tag
  (`v1.2.3`, `v1`). The parser distinguishes a registry host port from a tag
  separator by requiring the `:` to appear after the last `/`.
- No tag and no digest → falls back to the full `external_image_ref` string.

Full precedence, highest first:

| Priority | Value used as release |
|----------|----------------------|
| 1 (highest) | User-provided `SENTRY_RELEASE` in project env settings |
| 2 | `deployment.commit_sha` (when present) |
| 3 | Image tag extracted by `image_ref_release` (image deploys only) |
| 4 | Image digest extracted by `image_ref_release` (image deploys only) |
| 5 (lowest) | Full `external_image_ref` string (image deploys, tag/digest absent) |

### Why image deploys previously produced no usable release

Before this work, `plan_docker_image_deployment` injected no `SENTRY_RELEASE` at
all — only `plan_git_deployment` did. The SDK fell back to whatever default it
computed (often nothing, or a hard-coded `"dev"` in poorly-initialised apps), so
image-deployed services had no agreed release key and symbolication was structurally
impossible. The `or_insert_with` / `image_ref_release` logic is the fix.

### The compiled-binary trap

`SENTRY_RELEASE` works only when the SDK reads its release from the environment at
runtime. The Go and Rust Sentry SDKs accept a `ClientOptions.release` set in source
code — and when it is non-empty, they **do not** read `SENTRY_RELEASE`:

```go
// Go — baked into the binary; SENTRY_RELEASE is IGNORED.
sentry.Init(sentry.ClientOptions{ Release: "hardcoded-v1.2.3" })
```

If the hard-coded string doesn't match the artifact upload key, every frame
silently loses source context. The only safe patterns for compiled binaries are:

1. **Leave `release` empty** (zero value / `None`) so the SDK falls back to
   `SENTRY_RELEASE`, which Temps populates with the commit SHA or image tag.
2. **Inject the release at compile time** via linker flags / a build argument, and
   use the same value as the artifact upload key — e.g. Go
   `-ldflags "-X main.version=$VERSION"` with `VERSION="${CI_COMMIT_TAG:-$CI_COMMIT_SHA}"`.

Pattern 1 is preferred for services deployed on Temps: the platform owns release
injection and no code change is needed when the identifier changes. This is
app-side guidance — Temps cannot detect from the outside whether a compiled binary
is ignoring `SENTRY_RELEASE`; there is no platform-side mitigation.

## Decision

The release identity invariant for symbolication is:

**A stack frame has source context if and only if the SDK-reported `release`, the
artifact-upload `release`, and the platform-injected `SENTRY_RELEASE` are the same
immutable string.**

Temps enforces the platform side by:

1. Injecting `SENTRY_RELEASE` (and `OTEL_SERVICE_VERSION`) at deploy time in both
   `plan_git_deployment` and `plan_docker_image_deployment` using `or_insert_with`
   semantics that never overwrite a user-supplied value.
2. Deriving the injected value from `deployment.commit_sha` when available, falling
   back to the image tag or digest for image deploys, and to the full image
   reference only when no tag or digest is available.
3. Using the same `deployment.commit_sha` as the `release` key in the auto-capture
   jobs for git builds, so the captured artifact key matches the injected env var by
   construction.

The app-side of the invariant is documented guidance (not enforced):

- Interpreted runtimes: do not set `release` in `Sentry.init()`; let Temps inject
  it via `SENTRY_RELEASE`.
- Compiled binaries (Go/Rust): do not hard-code a non-empty `ClientOptions.release`.
  Leave it as the zero value / `None`, or bake the real commit/tag at build time and
  upload artifacts under the same string.
- Any manual upload of source maps or source files must use the same release string
  the deployed application will report.

This ADR documents a rule that already applies to the codebase, making it explicit
so future planner changes can't silently break it and so operators/SDK authors have
one reference for why releases must match.

## Consequences

### Positive

- Predictable join: while the invariant holds, every frame in a git-deployed JS or
  native service gets source context with zero developer configuration.
- Image-deployed services gain symbolication for the first time.
- `OTEL_SERVICE_VERSION` is set to the same string, so OTel spans carry the same
  build identity as error events — correlation across the two is trivial.
- Operators can override the platform's choice by setting `SENTRY_RELEASE` in env
  settings and uploading artifacts under that value.

### Negative

- For image deploys with no tag and no digest (a bare `registry.io/app`), the
  fallback is the full reference string — a weak identity; symbolication silently
  fails unless the operator sets `SENTRY_RELEASE` manually.
- The platform cannot detect a compiled binary that ignores `SENTRY_RELEASE`; the
  failure mode (events visible, source context never appears) is invisible until a
  developer compares uploaded releases against event release values.
- The stale-artifact cleanup (`delete_stale_source_maps` / `delete_stale_source_files`)
  reconstructs active releases as `commit_sha || "deploy-{id}"`. For image deploys
  where the release is a tag, tag-keyed artifacts would be treated as stale. This is
  a follow-up defect, not addressed here. (The native `CaptureSourceFilesJob` runs
  only on git builds, where the key is always `commit_sha`, so it is not currently
  affected.)

### Risks

- **User-override mismatch:** setting `SENTRY_RELEASE` to `v1.2.3` in env settings
  but uploading artifacts under `commit_sha` breaks the join; the platform cannot
  detect this.
- **Mutable image tags:** re-pushing `latest`/`stable` diverges the deployed image
  from uploaded artifacts without a new Temps deploy. Use immutable tags/digests.
- **Image deploys are manual:** the auto-capture jobs run only in
  `plan_git_deployment`. Image-deployed services must upload artifacts from their
  own CI (intentional — the build output is owned by external CI).

## Alternatives Considered

### Option A: Debug-ID keying (Sentry's DWARF/sourcemap debug-id approach)

Key artifacts by a content-addressed debug ID embedded in the artifact, reported by
events via `debug_meta`. Join becomes `(project_id, debug_id)`.

- **Pros:** eliminates the release-identity problem entirely; immune to
  mutable-tag and version-string drift; content-addressed.
- **Cons:** requires every JS build to inject the debug ID into both the map and
  the bundle (supported by `@sentry/bundler-plugin-core`, not by direct
  `sentry-cli` use or older plugins); the Go/Rust SDKs don't yet report
  `debug_meta` for native source context; requires schema + upload-API changes and
  a parallel migration path.

**Rejected for now** — the right long-term direction, tracked as a future follow-up
once release-keying is stable. Release-keying is simpler, compatible with all
existing Sentry plugins and the CLI, and is Sentry's own original baseline.

### Option B: Content-hash keying

Key by a SHA-256 of the artifact content, reported by the event.

**Rejected** — no SDK reports a source-map content hash; debug IDs (Option A) are
the content-addressable approach already progressing in the ecosystem.

### Option C: Release from OTel service version at ingest time

If `release_version` is absent, look up the OTel `service.version` from a recent
span for the same deployment token.

**Rejected** — introduces cross-domain coupling and ingest-hot-path latency, and
only helps the compiled-binary trap, whose root cause is app-side. `service.version`
and `SENTRY_RELEASE` are already set to the same value by the platform.

## Implementation Notes

- **No schema changes** — the `release` column already exists on `source_maps` and
  `source_files`.
- **No breaking changes** — `or_insert_with` preserves user-supplied
  `SENTRY_RELEASE`; image-deploy injection is additive.
- **Documentation:** operator guide (invariant, precedence, mutable-tag risk,
  compiled-binary trap, "no source context" troubleshooting); compiled-language SDK
  guide (correct/incorrect `ClientOptions.release`); CLI upload docs (`--release`
  must equal what the app reports).
- **Stale-cleanup defect (follow-up):** make the keep-set in `delete_stale_source_maps`
  / `delete_stale_source_files` include image-tag releases, not just `commit_sha`.

## References

- `crates/temps-deployments/src/services/workflow_planner.rs` — `plan_git_deployment`
  (`SENTRY_RELEASE` from `commit_sha`; `CaptureSourceMapsJob` / `CaptureSourceFilesJob`
  release key) and `plan_docker_image_deployment` (`image_ref_release`, precedence chain).
- `crates/temps-error-tracking/src/services/source_map_service.rs` — the
  `(project_id, release, file_path)` join shared by `resolve_frame` (JS) and
  `resolve_native_frame` (native); `symbolicate_error_event` release extraction;
  `delete_stale_source_maps` / `delete_stale_source_files`.
- PR #419 — native source context for compiled languages; introduces the
  `source_files` table, `upload_source_file` API, `resolve_native_frame`, the
  `error_source_context_enabled` flag, and the `CaptureSourceFilesJob` — all of which
  rely on this invariant.
