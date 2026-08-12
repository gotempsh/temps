# ADR-037: Sandbox Snapshots

**Status:** Proposed
**Date:** 2026-08-10
**Author:** David Viejo
**Related:** ADR-010 (provider boundary traits), ADR-029 (Firecracker backend), ADR-036 (persistent workspace sandboxes), ADR-013 (egress credential proxy)

## Context

Standalone sandboxes (`crates/temps-sandbox`, the `/v1/sandboxes` API) support creating a container from a preset image and seeding it from a git repo or tarball. Once a sandbox runs, its filesystem state evolves — packages are installed, config files are written, projects are compiled. Today there is no way to capture that state as a reusable artifact. Every new sandbox starts from the base image and must repeat setup from scratch.

Three use cases make this painful in practice:

1. **Checkpoint and restore.** A developer does 45 minutes of environment setup inside a workspace sandbox. Their trial period ends or they need to move to a different machine. Today the work is lost when the sandbox is destroyed; a snapshot would let them restore from exactly where they left off.

2. **Fork a sandbox.** An AI agent has assembled a working environment with a specific library version combination that took three retries to find. A second agent task should branch from that proven state, not rebuild it. Today the only option is to repeat the whole setup.

3. **Golden images.** An operator wants a pre-warmed sandbox image with company tooling already installed — avoid the 5-minute install step on every new sandbox. Today this requires maintaining a custom Dockerfile; a snapshot-from-running-sandbox flow removes that need.

The related deferred items add context. ADR-029 explicitly lists "snapshot-based pause" as out of scope for Firecracker v1. ADR-036 acknowledges that workspace disk is an unbounded resource, and snapshots make the accounting harder, not easier. ADR-013's credential scrubbing problem also applies to snapshots with extra severity: a snapshot is a persistent, reusable, potentially-shared artifact — any secret that makes it in is there forever.

The current `pause_sandbox`/`resume_sandbox` pair (`crates/temps-sandbox/src/services/sandbox_service.rs`) is not a snapshot. It calls `registry.stop()` and `registry.start()` — container stop/start — which preserves filesystem state only as long as the container exists and has not been destroyed. The state is not exportable, not copyable, and not recoverable after a `destroy`. This ADR adds the exportable, reusable artifact layer.

## Decision

Add sandbox snapshots as a first-class, provider-bounded feature. v1 supports the Docker backend only. Firecracker snapshot/restore is deferred to v2. Local and managed backends return an explicit "not supported" error rather than silently failing or degrading.

### 1. What "snapshot" means per backend

**Docker (`DockerSandboxProvider`).**
A snapshot is a committed image layer derived from a **running** container, produced by `docker commit` against the container filesystem. (v1 requires a running sandbox; snapshotting stopped sandboxes is a deferred v2 feature. If the sandbox is paused or stopped, resume it first.) The result is stored as a tagged image in the local Docker daemon under `temps-snapshot/<public_snapshot_id>:latest`, and simultaneously exported as a tarball to the content-addressed store under `$TEMPS_DATA_DIR/snapshots/<sha256-of-content>.tar`. The sha256 becomes the `content_digest` column on the `sandbox_snapshots` entity. Creating a sandbox from a Docker snapshot passes this image name as the `image` field of `SandboxCreateConfig`, which the existing `DockerSandboxProvider::create` path already handles. No new container-creation logic is needed for the restore side.

The Docker snapshot captures the container's writable layer only, not the mounted named volumes (`/home/temps`). The home volume contains user credentials and AI CLI state — these must not be snapshotted (see §4 Security). The work dir (`/workspace`) is part of the container writable layer and is captured. This is the intended division: workspace content is preserved, credentials are not.

**Firecracker (`FirecrackerSandboxProvider`).**
Firecracker has native snapshot/restore via its API (`/snapshot/create`, `/snapshot/load`). A Firecracker snapshot captures the full VM memory state plus disk states. This is significantly more powerful than Docker commit (it preserves in-flight processes, open file descriptors, and kernel state) but also more fragile: snapshot compatibility is pinned to the exact Firecracker version that produced it, and restoring requires re-seeding guest entropy and re-attaching network devices. ADR-029 §7 deferred this behind `firecracker.enable_snapshots`. **This ADR keeps that deferral.** Firecracker backends return `SandboxSnapshotError::NotSupported` from all snapshot trait methods until `enable_snapshots` is on and the restore safety work (entropy, clock, network re-attach, version pinning for portability) ships in a separate ADR.

**Local (`LocalSandboxProvider`).**
Local is a fork-exec dev-only fallback with no container primitives. It returns `SandboxSnapshotError::NotSupported` unconditionally. No snapshot concept applies.

**Managed (`managed.rs` / `RunSandboxService`).**
Managed run-sandboxes are agent-run lifecycle-managed and not user-accessible for snapshot. They return `SandboxSnapshotError::NotSupported`. An agent run's output is the PR/commit it produces, not a sandbox checkpoint.

### 2. New `SandboxProvider` trait methods

Two methods are added to the trait. Both carry default implementations that return `NotSupported`, preserving object-safety and requiring no changes to backends that do not implement them.

```
async fn take_snapshot(
    &self,
    handle: &SandboxHandle,
    label: Option<String>,
) -> Result<SnapshotArtifact, AgentError>
```

Captures the current state of `handle` as a reusable artifact. The sandbox may be running or stopped; the caller is responsible for any quiescing it needs (the service layer handles stopping-before-snapshot for Docker). Returns a `SnapshotArtifact` describing the on-disk location and content digest. `label` is a human-readable annotation stored on the DB row.

```
async fn create_from_snapshot(
    &self,
    artifact: &SnapshotArtifact,
    config: SandboxCreateConfig,
) -> Result<SandboxHandle, AgentError>
```

Creates and starts a new sandbox seeded from `artifact` instead of a base image. For Docker this translates to loading the snapshot image (if not already present in the daemon) and passing it as the `image` in `ContainerCreateBody`. The resulting handle is indistinguishable from one created from any other image — exec, file I/O, stop/start, and destroy all work unchanged.

```rust
pub struct SnapshotArtifact {
    /// Content-addressed path under $TEMPS_DATA_DIR/snapshots/<digest>.tar.
    /// The provider puts the artifact here; the service layer records it
    /// and manages lifecycle.
    pub content_path: std::path::PathBuf,
    /// SHA-256 of the tarball. The canonical store key — collisions among
    /// identical images are deduplicated on this field.
    pub content_digest: String,
    /// Approximate size in bytes.
    pub size_bytes: u64,
    /// Which backend produced this artifact — needed to reject
    /// cross-backend restore attempts (a Docker tar cannot boot as a
    /// Firecracker rootfs).
    pub backend: SandboxBackend,
}
```

`SnapshotArtifact` is a plain data struct, not a trait object — it carries only what the service layer needs to persist and re-hydrate. It does not leak provider-specific types across the boundary.

### 3. Storage: content-addressed artifacts under TEMPS_DATA_DIR

Snapshot tarballs land in `$TEMPS_DATA_DIR/snapshots/` as content-addressed files: `<sha256-hex>.tar`. The sha256 is computed by the provider as it streams out the export; the file is written atomically (write to a temp path, rename on close).

**Deduplication.** Two snapshots with the same content digest share one tarball on disk. The `sandbox_snapshots` entity has a `content_digest` column; the service checks for an existing row with the same digest before writing a new file. When a match is found, the new row points at the existing file and the write is skipped.

**Lifecycle and GC.** Snapshots are user-managed; there is no automatic GC. The `DELETE /v1/sandbox-snapshots/{snapshot_id}` endpoint removes the DB row and, if no other row references the same `content_digest`, deletes the tarball from disk. Operators running low on space can list snapshots sorted by size (`GET /v1/sandbox-snapshots?sort=size_bytes:desc`) and delete manually. A `GET /v1/sandbox-snapshots/storage-summary` endpoint reports total snapshot bytes on disk vs available disk, so the UI can show a warning before space runs out. Automatic GC (e.g. LRU eviction after N days unused) is explicitly left for a follow-up; the risk of silently destroying a user's checkpoint outweighs the operational tidiness. This mirrors the deliberate non-GC posture in the sandbox volume comment in `SandboxProvider` (`mod.rs:456-470`).

**Size concerns.** A snapshot of a Docker container writable layer is comparable in size to the filesystem changes the container has made on top of the base image — commonly 100 MB–2 GB for a development environment with compilers and node_modules. The `sandbox_snapshots` entity has a `size_bytes` column populated at create time. ADR-036 identified disk as the unbounded resource for persistent workspaces; snapshots compound that. **Quota enforcement is a hard requirement before Temps Cloud ships the snapshot feature.** v1 enforces a per-user soft cap as a config value on `AgentSandboxSettings` (`max_snapshot_bytes_per_user: u64`, defaulting to 10 GiB); exceeding it returns 422 with a descriptive error. A per-project cap is a natural follow-up once project-scoped quotas exist elsewhere.

The content store is local only in v1. Exporting snapshots to S3 / R2 for portability or off-host backup is explicitly deferred; the API shape (`content_digest` as the canonical key) is compatible with a remote store without migration.

### 4. Security: secret scrubbing before snapshot

**This is a hard requirement, not a nice-to-have.** A sandbox running with injected credentials contains several classes of secret that must not appear in a snapshot:

- **Credential daemon env file** (`/etc/temps/credential-daemon.env`) — injected at `exec_as_user` by the credential shim path. Contains the git provider token used to authenticate git operations inside the sandbox.
- **AI CLI credentials** (`/home/temps/.claude/` for Claude, analogous paths for other AI CLIs) — these live on the `/home/temps` named volume, which is deliberately *not* captured by a Docker commit (commit only captures the container writable layer, not mounts). This is by design and requires no scrubbing, but the design must be verified to hold for every backend.
- **Git credential bundle** (`crates/temps-agents/src/sandbox/git_credential_bundle.rs`) — the helper binary and daemon are in the image itself (compiled in); the actual tokens are written into `/etc/temps/credential-daemon.env` at runtime. The binary is fine in a snapshot; the env file is not.
- **Injected env vars** (`SandboxCreateConfig::env_vars`) — these are passed via Docker's `ContainerCreateBody.env` and are stored in the container's OCI config, which `docker commit` preserves. An `ANTHROPIC_API_KEY` or `GITHUB_TOKEN` passed at create time will be in the snapshot unless explicitly removed.

**Scrubbing protocol for Docker (v1):**

Before calling `docker commit`, `DockerSandboxProvider::take_snapshot` executes three scrubbing steps:

1. `exec_as_root` to shred and remove `/etc/temps/credential-daemon.env`.
2. Zero every known-sensitive env-var value in the committed image config by passing `ENV KEY=` Dockerfile instructions via Docker's `changes` query parameter (`docker commit --change 'ENV KEY='` for each sensitive key). This **zeroes** each value to an empty string — it is the only mechanism the Docker commit API actually supports. The Docker Engine silently ignores the `ContainerConfig` body's `env` field (verified against a real Docker daemon: passing `ContainerConfig { env: ... }` to the commit API has zero effect on the committed image's `Config.Env`). Each sensitive key remains present in the committed image with an empty value (`KEY=`); the Docker commit API has no mechanism to delete an env entry entirely.
3. Verify scrubbing by inspecting the committed image's `Config.Env` and rejecting the snapshot if any known-sensitive key has a **non-empty** value (reject, not silently proceed). A key present with an empty value (`KEY=`) is considered successfully scrubbed.

The credential daemon env file scrubbing leaves a window between the scrub exec and the commit during which the file is gone but the running credential daemon may still hold the token in memory. This is acceptable for snapshot purposes — the window affects only the next git operation inside that sandbox, not the snapshot artifact. If the sandbox continues running after snapshot, the injector must re-write the credential file (the service layer handles this by re-running the credential injection step after snapshot completes, if the sandbox was left running).

**The home volume is not snapshotted.** `docker commit` captures only the container's own writable layer — named volumes mounted at `/home/temps` are excluded. This is the correct behaviour for AI CLI credentials, shell history, and Claude auth tokens. The ADR documents this explicitly so no future refactor quietly changes the behaviour (e.g., switching to `docker export` which *does* include mount points of `/proc`, though not bind mounts — the distinction is subtle and must be guarded by a test).

**Security sign-off.** The scrubbing logic and the test that verifies no known-credential-key appears in a committed snapshot image must be reviewed by the `security-auditor` agent before this ADR is marked Accepted and the PR is merged.

### 5. DB schema: `sandbox_snapshots` entity

A new `crates/temps-entities/src/sandbox_snapshots.rs` entity and migration.

```
sandbox_snapshots
  id              INTEGER PRIMARY KEY (monotonic internal)
  public_id       VARCHAR UNIQUE NOT NULL  -- e.g. "snap_a1b2c3d4e5f6"
  user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE
  project_id      INTEGER  -- nullable, no FK (mirrors sandboxes.project_id policy)
  source_sandbox_id INTEGER  -- nullable, no FK -- the sandbox this was snapped from; NULL if the sandbox was later destroyed
  label           VARCHAR  -- user-supplied human label
  status          VARCHAR NOT NULL  -- 'creating' | 'ready' | 'failed' | 'deleted'
  backend         VARCHAR NOT NULL  -- 'docker' | 'firecracker' -- cross-backend restore is rejected
  content_digest  VARCHAR NOT NULL  -- sha256 of the artifact tarball, the dedup key
  content_path    VARCHAR NOT NULL  -- absolute path on the host filesystem
  size_bytes      BIGINT NOT NULL DEFAULT 0
  image_ref       VARCHAR  -- for Docker: the daemon image tag temps-snapshot/<public_id>:latest
  metadata        JSONB  -- reserved for backend-specific fields without migrations
  created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
  updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
```

**`source_sandbox_id` is not a foreign key.** The sandbox that was snapped may later be destroyed; that must not cascade into destroying the snapshot. The column is a display aid, not a referential constraint. It is set to NULL (not deleted) when the source sandbox is destroyed, via an explicit `ON DELETE SET NULL` trigger or by the sandbox destroy service path updating the column.

Wait — actually, to simplify: the source_sandbox_id nullable column with no FK, exactly as `sandboxes.project_id` is modelled. The destroy path in `SandboxService::destroy_sandbox` nullifies any referencing `sandbox_snapshots.source_sandbox_id` rows before removing the sandbox row. This avoids a trigger and keeps the behaviour explicit.

**`status` state machine:**
- `creating` — snapshot operation in progress; the artifact is being written.
- `ready` — artifact on disk, digest verified; the snapshot is usable.
- `failed` — the operation failed; the row is kept for audit; artifact may be absent.
- `deleted` — soft-deleted; artifact removed from disk. Hard deletes are not used for audit safety.

**Index:** `(user_id, status, created_at DESC)` — the primary list query pattern. `(content_digest)` unique partial index over `status = 'ready'` for dedup lookups.

**Relation to `sandboxes`:** "create sandbox from snapshot" does not add a FK from `sandboxes` to `sandbox_snapshots`. A sandbox created from a snapshot is thereafter indistinguishable from one created from a plain image — it does not "belong" to the snapshot in any ongoing sense. The snapshot's `public_id` may be recorded in `sandboxes.metadata` for display, but without a referential constraint (destroying the snapshot must not affect an already-running sandbox that was created from it).

### 6. API surface

All endpoints live in `crates/temps-sandbox` and follow the three-layer pattern (Handler → Service → Data Access).

**Handler layer:** `crates/temps-sandbox/src/handlers/snapshots.rs`
**Service layer:** `crates/temps-sandbox/src/services/snapshot_service.rs`
**Data access:** Sea-ORM queries over `sandbox_snapshots::Entity` in `crates/temps-entities`

Error type: `SandboxSnapshotError` (new enum in `crates/temps-sandbox/src/error.rs`), with `From<SandboxSnapshotError> for Problem` in the handler module.

```
POST   /v1/sandboxes/{sandbox_id}/snapshots
GET    /v1/sandbox-snapshots
GET    /v1/sandbox-snapshots/{snapshot_id}
DELETE /v1/sandbox-snapshots/{snapshot_id}
GET    /v1/sandbox-snapshots/storage-summary
POST   /v1/sandboxes          (existing — gains optional `from_snapshot` field)
```

**`POST /v1/sandboxes/{sandbox_id}/snapshots`** — takes a snapshot of the sandbox identified by `sandbox_id`. Request body: `{ "label": "optional human name" }`. The handler:
1. Verifies the caller owns the sandbox.
2. Creates a `sandbox_snapshots` row in `creating` status.
3. Calls `SnapshotService::take_snapshot` which: stops the sandbox if running (to quiesce the filesystem), calls `provider.take_snapshot(handle, label)`, scrubs credentials (see §4), writes the artifact, verifies the digest, transitions the row to `ready`.
4. Returns `202 Accepted` with the snapshot row (status `creating`) immediately. The snapshot operation is long — potentially minutes for a large container. The actual work runs in a background task (using the existing `JobTracker` pattern) and the caller polls the snapshot row for status. The response includes a `Location` header pointing at the snapshot resource.

The sandbox must be running when the snapshot is requested (v1 constraint — stopped-sandbox snapshots are deferred to v2). The sandbox is stopped for the duration of the snapshot operation (for Docker commit consistency) and restarted when the snapshot completes. The caller sees the sandbox status flip to `stopped` and back to `running` as a normal lifecycle event.

**`GET /v1/sandbox-snapshots`** — list snapshots owned by the authenticated user. Query params: `project_id`, `status`, `page`, `page_size` (default 20, max 100). Returns paginated `sandbox_snapshots` rows with `size_bytes` and `status` visible.

**`GET /v1/sandbox-snapshots/{snapshot_id}`** — fetch a single snapshot row.

**`DELETE /v1/sandbox-snapshots/{snapshot_id}`** — soft-deletes the snapshot row (sets `status = 'deleted'`) and, if no other `ready` row shares the `content_digest`, removes the tarball from disk. Idempotent.

**`GET /v1/sandbox-snapshots/storage-summary`** — returns `{ total_bytes: u64, snapshot_count: u32, quota_bytes: u64, available_disk_bytes: u64 }`. Used by the UI to show a storage warning and by the service to enforce the per-user quota.

**`POST /v1/sandboxes` gains `from_snapshot`:**

```jsonc
{
  "from_snapshot": "snap_a1b2c3d4e5f6",   // optional; mutually exclusive with `image`
  "label": "my restored workspace",
  "lifecycle": "workspace"
  // ... other fields unchanged
}
```

When `from_snapshot` is set:
- `image` must be absent (error 400 if both are set).
- `source` may still be set to re-seed the work dir from a git repo *after* the snapshot boots, but this is unusual and the documentation should discourage it.
- The service resolves the snapshot row, verifies the caller owns it (or it is project-scoped and the caller has project access), verifies `status == 'ready'` and `backend` matches the target backend, calls `provider.create_from_snapshot(artifact, config)`, and proceeds as a normal sandbox create from there.
- Cross-backend restore is rejected with 422: a Docker snapshot artifact cannot be used with a Firecracker backend.

**Error variants for `SandboxSnapshotError`:**

```rust
NotFound { snapshot_id: String },
NotReady { snapshot_id: String, status: String },
CrossBackendRestore { snapshot_backend: String, target_backend: String },
NotSupported { backend: String },
QuotaExceeded { user_id: i32, used_bytes: u64, quota_bytes: u64 },
ScrubFailed { sandbox_id: String, reason: String },
DigestMismatch { expected: String, actual: String },
ArtifactMissing { path: String },
SandboxNotFound { sandbox_id: String },
InvalidState { sandbox_id: String, state: String, operation: String },
Database(#[from] sea_orm::DbErr),
Io(#[from] std::io::Error),
Provider(#[from] AgentError),
```

HTTP mappings follow the standard pattern: `NotFound` → 404, `NotReady` / `CrossBackendRestore` / `QuotaExceeded` / `InvalidState` → 422, `NotSupported` → 501, others → 500.

All write endpoints require `permission_guard!(auth, SandboxesCreate)` (snapshots are a sub-capability of sandbox management). A dedicated `SnapshotsManage` permission can be split out later if operators need to restrict snapshotting independently.

All write endpoints produce audit log entries via `AuditService`.

### 7. CLI parity (`apps/temps-cli`)

New subcommands under `temps sandbox`:

```
temps sandbox snapshot <sandbox-id> [--label <label>] [--wait]
  Creates a snapshot of a sandbox. Prints the snapshot ID.
  --wait: polls until status == 'ready' or 'failed', showing progress.

temps sandbox snapshots list [--project <project-id>] [--status <status>]
  Lists snapshots for the authenticated user, tabular output.

temps sandbox snapshots show <snapshot-id>
  Shows detail of a single snapshot including size and status.

temps sandbox snapshots delete <snapshot-id>
  Deletes a snapshot and reclaims its disk if no other snapshot shares the digest.

temps sandbox snapshots storage
  Shows quota usage summary.

temps sandbox create --from-snapshot <snapshot-id> [--label <label>] [--lifecycle workspace]
  Creates a new sandbox from a snapshot. Existing --image flag is mutually
  exclusive; the CLI enforces this at argument parse time and reports a clear
  error if both are supplied.
```

These commands use the TypeScript generated client (`apps/temps-cli/src/api/`) and follow the existing command structure in `crates/temps-cli/src/commands/sandbox.rs`. The OpenAPI spec must be regenerated (`bun run spec:update`, `bun run generate:api`) after the new endpoints land.

### 8. Relationship to existing pause/resume

The existing pause/resume (`POST /v1/sandboxes/{id}/pause`, `POST /v1/sandboxes/{id}/resume`) is container stop/start — it preserves filesystem state implicitly as long as the container exists, but produces no exportable artifact and is not recoverable after destroy. **This ADR does not change or deprecate pause/resume.** They serve a different and simpler purpose: temporarily stopping a running sandbox to save host resources, with a guaranteed restart path. They are cheap, instant, and correct for their use case.

Snapshots and pause/resume are complementary, not competing. The recommended mental model: pause = "stop what you're doing and come back later on the same machine"; snapshot = "capture where you are so you can restore on this machine or start a fork".

A future ADR could optionally make workspace sandbox suspension (ADR-036 §2, the sweeper stopping a workspace on idle) use a snapshot instead of a bare stop. That would allow true cross-host workspace portability. This is explicitly out of scope for ADR-037. The `pause_sandbox` / `resume_sandbox` code paths are not touched by this ADR.

### 9. Phasing

**v1 (this ADR, first PR):** Docker backend only. Snapshot via `docker commit` + tarball export. Storage in `$TEMPS_DATA_DIR/snapshots/`. Credential scrubbing for env vars and `/etc/temps/credential-daemon.env`. Per-user quota enforcement. Full API surface (create, list, show, delete, storage-summary, create-from-snapshot). CLI parity. Security auditor sign-off on scrubbing logic and test coverage. No Firecracker snapshot, no S3/R2 export, no automatic GC, no cross-host portability.

**v2 (separate ADR, after Firecracker v1 is in production):** Firecracker snapshot/restore via the Firecracker snapshot API. Requires restore-safety work: guest entropy re-seed on restore, clock skew handling, network device re-attach, Firecracker version pin enforcement (snapshot format is not portable across Firecracker major versions). The `SnapshotArtifact` struct already carries `backend` to distinguish Docker vs Firecracker artifacts; no DB migration needed for v2 if `metadata` JSONB carries the Firecracker-specific fields (memory snapshot path, VM state path, disk diff paths).

**v3 (future):** Remote artifact store (S3 / R2) for off-host backup and cross-host restore. The content-addressed design (`content_digest` as canonical key) is compatible with a remote store without schema migration. Automatic GC with per-user retention policies. Sharing snapshots across a team (project-scoped snapshots visible to all project members).

**Explicitly out of scope for all phases addressed here:**
- Snapshots of agent-run sandboxes (managed lifecycle, not user-accessible).
- Snapshot export to OCI registries (separate design; `docker commit` output is already an OCI layer, but push-to-registry adds auth/registry concerns).
- Incremental snapshots (delta from a previous snapshot). The content-addressed store could support this, but the dedup benefit does not justify the complexity for v1.
- Automatic periodic snapshots (cron-like). Operator-driven only.
- Live snapshot of a running sandbox without stopping (Docker's live commit is possible but filesystem consistency is not guaranteed without quiescing; requiring a stop is the safe default).

## Consequences

### Positive

- Developers can checkpoint expensive environment setup and restore from it without repeating work. This directly unlocks the "fork a workspace" flow for AI agent tasks.
- ADR-010 pays off: handlers and services in `temps-sandbox` never touch bollard directly. The provider boundary absorbs the backend difference.
- The content-addressed store deduplicates identical snapshots at zero extra cost, which matters when a team is creating many sandboxes from the same base environment.
- The scrubbing protocol is explicit, auditable, and tested — it does not rely on "we didn't inject credentials" being true by convention.
- CLI parity ships in v1, maintaining the invariant that every API endpoint has a CLI counterpart in the same PR.

### Negative

- Disk consumption is the primary operational risk. A single large snapshot (a workspace with build outputs) can be 5–10 GB. The per-user quota is a soft cap enforced at API level, not a hard block at the filesystem level. An operator on a small host (cpx22, 40 GB disk) can exhaust disk with a handful of unmanaged snapshots. The storage-summary endpoint and UI warning are the only guards in v1.
- Snapshot creates a stop-restart cycle visible to the sandbox user. For a workspace with a live terminal session, the terminal connection drops during snapshot and must be re-established after restart. ADR-036's heartbeat mechanism (`touch` every 20s on attached terminal) means the sandbox was not being swept while in use, but the snapshot-induced stop looks like a sweep to the terminal client. The API response must communicate this clearly.
- The scrubbing step adds latency before commit. For a container with many env vars, inspecting and rewriting the committed image config takes seconds. This is bounded and acceptable for an async operation.
- Docker image tags (`temps-snapshot/<public_id>:latest`) accumulate in the local daemon. The `DELETE` endpoint must also call `docker rmi` on the image tag. The `storage-summary` endpoint should report daemon image size separately from tarball size to give operators a complete picture.
- Cross-backend restore rejection (Docker snapshot → Firecracker backend) is a UX friction point for operators who mix backends. The error message must explain the incompatibility and suggest re-snapping from a sandbox on the target backend.

### Risks

- **Credential leakage into snapshot.** The scrubbing protocol covers known credential paths. Unknown paths (e.g., a user's own `.netrc`, ssh keys written to `/root/.ssh` inside the container by a user script) are not scrubbed. The documentation must make clear that user-written secrets outside the known paths are the user's responsibility to remove before snapshotting. A pre-snapshot warning in the UI ("review that no secrets are stored in the container filesystem before creating a snapshot") is the minimum mitigation.
- **Digest collision.** SHA-256 collision is effectively impossible in practice, but the dedup logic must handle the case where two different content byte streams produce the same digest hash (the test should verify this path returns an error rather than silently corrupting a stored artifact).
- **Partial artifact.** If the server crashes between writing the tarball and updating the `sandbox_snapshots` row to `ready`, a dangling tarball is left on disk. A startup sweep that removes tarballs with no corresponding `ready` row (scoped to files older than 1 hour, to avoid racing with in-progress creates) handles this. The `snapshot_id` is embedded in the temp path until rename, so the sweep can identify orphans.
- **`docker commit` on a paused container.** Docker allows commit on a stopped container cleanly. On a running container, there is a TOCTOU window. v1 requires the sandbox to be stopped before commit; the service enforces this.

## Alternatives Considered

### Option A: Snapshot via `docker export` instead of `docker commit`

`docker export` produces a flat rootfs tarball rather than an OCI image layer. Pro: no Docker daemon dependency on restore — any tool that can unpack a tar and build an image can use it. Con: loses the Docker image metadata (ENV, WORKDIR, ENTRYPOINT) that `docker commit` preserves; restoring requires a `docker import` step to reconstruct the image config, which is more fragile. `docker commit` is the right primitive for "save and restore a running container's state as a reusable image".

### Option B: Named volume snapshot for home dir

Snapshot the `/home/temps` named volume (Docker's volume backup pattern: `docker run --rm --volumes-from <container> tar c /home/temps`) in addition to or instead of the container writable layer. This would capture AI CLI credentials, shell history, and Claude auth state. Rejected: the whole point of the home volume split is that credentials live in a place that is not snapshotted. Snapshotting the home volume captures exactly what we must not capture without a separate and harder scrubbing protocol. The home volume backup use case is served by the user's own explicit backup (operator-managed volume backup), not by the sandbox snapshot API.

### Option C: Snapshot as OCI image push to a registry

Push the `docker commit` result directly to a container registry (local or remote) rather than writing a tarball to disk. Pro: reuse existing registry infrastructure and tooling; snapshots are already versioned and pullable. Con: adds registry auth complexity (the user's Docker daemon credentials, or Temps-managed registry credentials, or a Temps-run local registry), makes the storage-summary harder to compute, and complicates the per-user quota enforcement. This is the right v3 direction for cross-host portability, not the right v1 path.

### Option D: Pause/resume becomes snapshot-based

Replace the current container stop/start with Firecracker-style memory snapshots (or CRIU for Docker). This would make pause/resume cheaper and more portable. Rejected: CRIU on Docker is experimental and frequently broken on modern kernels; it requires root and has poor support for containerized workloads with network sockets. Firecracker snapshot for pause is deferred in ADR-029 for exactly these reasons. Conflating the two features would block the simple and working pause/resume behind the hard and risky snapshot machinery. They stay separate.

## Implementation Notes

- **Affected crates:** `crates/temps-entities` (new entity + migration), `crates/temps-sandbox` (new service, new handler, error type extension), `crates/temps-agents` (two new default-impl methods on `SandboxProvider`, `SnapshotArtifact` struct, concrete impl in `docker.rs`), `apps/temps-cli` (new subcommands), `crates/temps-migrations` (new migration).
- **Migration needed:** yes — new `sandbox_snapshots` table and index.
- **Breaking changes:** no — new methods on `SandboxProvider` have default implementations returning `NotSupported`; existing backends compile unchanged. The new `from_snapshot` field on `CreateSandboxRequest` is optional and `#[serde(default)]`.
- **Provider boundary check:** `scripts/check-provider-boundary.sh` must continue to pass — `temps-sandbox` must not import `bollard` directly. The snapshot logic that calls `docker commit` lives in `crates/temps-agents/src/sandbox/docker.rs` behind the `SandboxProvider::take_snapshot` method. The service in `temps-sandbox` calls the trait method only.
- **Security auditor sign-off required** on the credential scrubbing logic and the test that verifies no known-sensitive env key survives a snapshot commit.
- **Eval harness** in `temps-agents/tests/` must gain snapshot test cases (take snapshot, delete original sandbox, create from snapshot, verify exec works) gated on Docker availability, following the existing pattern.

## References

- ADR-008 — in-sandbox PTY agent (terminal drop-during-snapshot user impact)
- ADR-009 — sandbox API versioning (optional `from_snapshot` field is non-breaking)
- ADR-010 — provider boundary traits (why snapshot logic lives in `docker.rs` not `temps-sandbox`)
- ADR-013 — sandbox egress credential proxy (threat model for credential scrubbing)
- ADR-029 — Firecracker sandbox backend (why Firecracker snapshot is deferred; §7 "Pause/resume v2")
- ADR-036 — persistent workspace sandboxes (disk as unbounded resource; workspace lifecycle context)
- `crates/temps-agents/src/sandbox/mod.rs` — `SandboxProvider` trait (object-safety constraint; default-impl pattern)
- `crates/temps-agents/src/sandbox/docker.rs` — reference backend implementation
- `crates/temps-sandbox/src/services/sandbox_service.rs` — `pause_sandbox`/`resume_sandbox` (what snapshots are not)
- `crates/temps-entities/src/sandboxes.rs` — `sandboxes` entity (modelling reference for FK-less project_id)
- Docker: `docker commit` API, `--change` flag for ENV rewrite
- Firecracker: snapshot/restore API — https://github.com/firecracker-microvm/firecracker/blob/main/docs/snapshotting/snapshot-support.md
