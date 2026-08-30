# ADR-037: Sandbox Snapshots

**Status:** Accepted
**Date:** 2026-08-10
**Updated:** 2026-08-29
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

Add sandbox snapshots as a first-class, provider-bounded feature. Docker and Firecracker both support persistent filesystem snapshots. Local and managed backends return an explicit "not supported" error rather than silently failing or degrading.

### 1. What "snapshot" means per backend

**Docker (`DockerSandboxProvider`).**
A snapshot is a committed image layer derived from a **running** container, produced by `docker commit` against the container filesystem. (v1 requires a running sandbox; snapshotting stopped sandboxes is a deferred v2 feature. If the sandbox is paused or stopped, resume it first.) The result is stored as a content-addressed tag in the local Docker daemon under `temps-snapshot/<logical_digest>:latest`, and simultaneously exported as a tarball under `$TEMPS_DATA_DIR/snapshots/<logical_digest>.tar`. The logical digest combines the primary image-tar digest with the companion workspace digest and becomes the `content_digest` column. Restore verifies both artifacts and the immutable Docker image ID, imports the image tar when necessary, restores the workspace, and then delegates container creation to the normal Docker path.

The Docker image snapshot captures the container's writable layer but excludes mounts. The home volume (`/home/temps`) contains user credentials and AI CLI state and remains excluded. The workspace is a separate bind mount, so the provider creates a companion workspace tarball, stores its digest/path in snapshot metadata, and restores it into the new sandbox work directory before container creation.

**Firecracker (`FirecrackerSandboxProvider`).**
Firecracker snapshots capture persistent filesystem state, not live VM memory. The service stops the VM, then `debugfs` exports live directory entries from the ext4 root disk inside a networkless minimal bubblewrap mount namespace. Only system binaries/libraries are mounted read-only; procfs is absent, `/dev` and `/run` are empty directories on an otherwise read-only root, and the dedicated staging directory is the sole host-writable mount. That staging directory is an unprivileged `fuse2fs` mount of a private ext4 scratch image whose total capacity equals the caller's remaining snapshot quota, so sparse guest files and hard-link amplification fail with `ENOSPC` before they can exceed the host-write budget. The narrow `fusermount3` helper handles teardown; the Temps daemon does not require root or `CAP_SYS_ADMIN`. Extraction also has a 15-minute timeout with forced child termination and cleanup. Parser-controlled stdin, stdout, and stderr are discarded rather than buffered. Host configuration, home directories, sysfs, runtime sockets/devices, and temps data are absent, containing malicious guest dirent traversal (e2fsprogs#272) and limiting parser compromise. Runtime-secret locations (`/root`, `/etc/temps`, `/tmp`, `/run`, `/var/tmp`) are removed, and `mkfs.ext4` builds a fresh sparse filesystem artifact. Rebuilding rather than copying the raw block device excludes deleted credential blocks and avoids Firecracker memory-snapshot compatibility, entropy, clock, and network-state hazards. `/workspace` is part of this sanitized filesystem and is restored with it.

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
    max_size_bytes: u64,
) -> Result<SnapshotArtifact, AgentError>
```

Captures the current state of `handle` as a reusable artifact. The service layer quiesces the sandbox first. `max_size_bytes` is the caller's remaining quota and is a hard streaming/capture limit; crossing it aborts and cleans up the attempt.

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
    /// Logical SHA-256 store key. Docker combines its image and workspace
    /// digests; Firecracker uses the rootfs digest directly.
    pub content_digest: String,
    /// SHA-256 of the primary artifact file (the logical digest may combine
    /// primary and companion digests).
    pub primary_digest: String,
    /// Total artifact size in bytes.
    pub size_bytes: u64,
    /// Which backend produced this artifact — needed to reject
    /// cross-backend restore attempts (a Docker tar cannot boot as a
    /// Firecracker rootfs).
    pub backend: SandboxBackend,
    /// Docker's mutable tag and immutable image ID.
    pub image_ref: Option<String>,
    pub image_id: Option<String>,
    /// Docker workspace companion; Firecracker stores workspace in ext4.
    pub workspace: Option<SnapshotCompanionArtifact>,
}
```

`SnapshotArtifact` is a plain data struct, not a trait object — it carries only what the service layer needs to persist and re-hydrate. It does not leak provider-specific types across the boundary.

### 3. Storage: content-addressed artifacts under TEMPS_DATA_DIR

Snapshot artifacts land in `$TEMPS_DATA_DIR/snapshots/` as content-addressed files. Docker writes an image tarball plus a workspace companion; Firecracker writes a sanitized `.ext4` artifact. Files publish without replacing an existing digest path.

**Deduplication.** Two snapshots with the same logical content digest share the same artifact set on disk: one ext4 file for Firecracker, or the primary image tar plus workspace companion for Docker. The `sandbox_snapshots` entity has a `content_digest` column; the service checks for an existing ready row with the same digest and reuses its persisted paths and integrity metadata.

**Lifecycle and GC.** Snapshots are user-managed; there is no automatic GC. The `DELETE /v1/sandbox-snapshots/{snapshot_id}` endpoint removes the DB row and, if no other row references the same `content_digest`, deletes all associated artifacts from disk. Capture publication/finalization, restore consumption, and delete reference counting/removal share a process-wide lifecycle mutex so a shared artifact cannot be removed or orphaned by concurrent requests. This is correct for temps' single-binary deployment model; a future multi-process deployment must replace it with a database advisory lock or transactional artifact-reference table. Operators running low on space can use the storage-summary endpoint and delete snapshots manually. Automatic GC (for example LRU eviction) is explicitly deferred because silently destroying a user's checkpoint is worse than requiring operator cleanup.

**Size concerns.** The service computes each user's remaining quota (default 10 GiB) before capture and passes it into the provider. Docker bounds both the image export stream and workspace tar writer; Firecracker extracts into a scratch ext4 filesystem capped to that remaining quota, then also validates allocated source bytes, exported live files, filesystem sizing, and final allocated bytes. Exceeding the limit aborts before publication and returns 422. The service rechecks the reported total before finalizing the row.

The content store is local only in v1. Exporting snapshots to S3 / R2 for portability or off-host backup is explicitly deferred; the API shape (`content_digest` as the canonical key) is compatible with a remote store without migration.

### 4. Security: secret scrubbing before snapshot

**This is a hard requirement, not a nice-to-have.** A sandbox running with injected credentials contains several classes of secret that must not appear in a snapshot:

- **Credential daemon env file** (`/etc/temps/credential-daemon.env`) — injected at `exec_as_user` by the credential shim path. Contains the git provider token used to authenticate git operations inside the sandbox.
- **AI CLI credentials** (`/home/temps/.claude/` for Claude, analogous paths for other AI CLIs) — these live on the `/home/temps` named volume, which is deliberately *not* captured by a Docker commit (commit only captures the container writable layer, not mounts). This is by design and requires no scrubbing, but the design must be verified to hold for every backend.
- **Git credential bundle** (`crates/temps-agents/src/sandbox/git_credential_bundle.rs`) — the helper binary and daemon are in the image itself (compiled in); the actual tokens are written into `/etc/temps/credential-daemon.env` at runtime. The binary is fine in a snapshot; the env file is not.
- **Injected env vars** (`SandboxCreateConfig::env_vars`) — these are passed via Docker's `ContainerCreateBody.env` and are stored in the container's OCI config, which `docker commit` preserves. An `ANTHROPIC_API_KEY` or `GITHUB_TOKEN` passed at create time will be in the snapshot unless explicitly removed.

**Scrubbing protocol for Docker (v1):**

Before and during `docker commit`, the snapshot flow executes three scrubbing steps:

1. `SnapshotService` calls `exec_as_root` to shred and remove `/etc/temps/credential-daemon.env` before stopping the sandbox.
2. Zero every known-sensitive env-var value in the committed image config by passing `ENV KEY=` Dockerfile instructions via Docker's `changes` query parameter (`docker commit --change 'ENV KEY='` for each sensitive key). This **zeroes** each value to an empty string — it is the only mechanism the Docker commit API actually supports. The Docker Engine silently ignores the `ContainerConfig` body's `env` field (verified against a real Docker daemon: passing `ContainerConfig { env: ... }` to the commit API has zero effect on the committed image's `Config.Env`). Each sensitive key remains present in the committed image with an empty value (`KEY=`); the Docker commit API has no mechanism to delete an env entry entirely.
3. Verify scrubbing by inspecting the committed image's `Config.Env` and rejecting the snapshot if any known-sensitive key has a **non-empty** value (reject, not silently proceed). A key present with an empty value (`KEY=`) is considered successfully scrubbed.

The credential daemon env file scrubbing leaves a window between the scrub exec and the commit during which the file is gone but the running credential daemon may still hold the token in memory. This is acceptable for snapshot purposes — the window affects only the next git operation inside that sandbox, not the snapshot artifact. If the sandbox continues running after snapshot, the injector must re-write the credential file (the service layer handles this by re-running the credential injection step after snapshot completes, if the sandbox was left running).

**The home volume is not snapshotted.** `docker commit` captures only the container's own writable layer — named volumes mounted at `/home/temps` are excluded. This is the correct behaviour for AI CLI credentials, shell history, and Claude auth tokens. The ADR documents this explicitly so no future refactor quietly changes the behaviour (e.g., switching to `docker export` which *does* include mount points of `/proc`, though not bind mounts — the distinction is subtle and must be guarded by a test).

**Firecracker does not copy the raw root disk.** Unlinking or shredding a file on ext4 is not proof that its blocks are unrecoverable. Firecracker snapshot capture therefore exports only live filesystem entries, removes runtime credential directories, and creates a fresh ext4 artifact. User-authored files under `/workspace` are intentionally preserved; platform-injected `/root` and `/etc/temps` state is not.

**Restore integrity.** Every primary and companion artifact is SHA-256 verified before restore. Docker additionally records the immutable image ID and refuses to trust a mutable daemon tag without an exact ID match. Missing integrity metadata fails closed.

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
  content_digest  VARCHAR NOT NULL  -- sha256 logical snapshot key, used for dedup
  content_path    VARCHAR NOT NULL  -- absolute path on the host filesystem
  size_bytes      BIGINT NOT NULL DEFAULT 0
  image_ref       VARCHAR  -- for Docker: the daemon image tag temps-snapshot/<logical_digest>:latest
  metadata        JSONB  -- primary digest, immutable image ID, companion metadata
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

**Index:** `(user_id, status, created_at DESC)` — the primary list query pattern. `(content_digest)` non-unique partial index over `status = 'ready'` accelerates dedup lookups while allowing separate user-owned rows to share one artifact.

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
3. Calls `SnapshotService::create_snapshot`, which stops the sandbox, scrubs credentials, calls `provider.take_snapshot(handle, label, remaining_quota)`, persists verified artifact metadata, and transitions the row to `ready`.
4. Returns the completed snapshot row. Capture currently runs in the request lifecycle; moving it to a durable background job remains future work.

The sandbox must be running when the snapshot is requested (v1 constraint — stopped-sandbox snapshots are deferred to v2). The sandbox is stopped for the duration of the snapshot operation (for Docker commit consistency) and restarted when the snapshot completes. The caller sees the sandbox status flip to `stopped` and back to `running` as a normal lifecycle event.

**`GET /v1/sandbox-snapshots`** — list snapshots owned by the authenticated user. Query params: `project_id`, `status`, `page`, `page_size` (default 20, max 100). Returns paginated `sandbox_snapshots` rows with `size_bytes` and `status` visible.

**`GET /v1/sandbox-snapshots/{snapshot_id}`** — fetch a single snapshot row.

**`DELETE /v1/sandbox-snapshots/{snapshot_id}`** — soft-deletes the snapshot row (sets `status = 'deleted'`) and, if no other `ready` row shares the `content_digest`, removes its primary/companion artifact set and content-addressed Docker tag. Idempotent.

**`GET /v1/sandbox-snapshots/storage-summary`** — returns `{ total_bytes: u64, snapshot_count: u32, quota_bytes: u64, available_disk_bytes: Option<u64> }`. `null` currently means host availability is unknown. The response is used by the UI to show storage usage; the service enforces the per-user quota from persisted ready-snapshot sizes.

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
- `source` must be absent because the snapshot already owns workspace contents. `project_id` remains valid attribution but does not implicitly clone over the restored workspace.
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

**v1:** Docker snapshot via committed image export plus an explicit workspace companion. Storage in `$TEMPS_DATA_DIR/snapshots/`, credential scrubbing, hard per-capture quota enforcement, and restore-time integrity verification.

**v2 (current):** Firecracker persistent filesystem snapshot/restore via sanitized ext4 reconstruction. This deliberately does not use Firecracker's live memory snapshot API; memory-state pause remains deferred.

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

- Disk consumption remains the primary operational risk. Capture uses a hard remaining-quota limit and cleanup on failure, but operators still need enough headroom for temporary files during capture.
- Snapshot creates a stop-restart cycle visible to the sandbox user. For a workspace with a live terminal session, the terminal connection drops during snapshot and must be re-established after restart. ADR-036's heartbeat mechanism (`touch` every 20s on attached terminal) means the sandbox was not being swept while in use, but the snapshot-induced stop looks like a sweep to the terminal client. The API response must communicate this clearly.
- The scrubbing step adds latency before commit. For a container with many env vars, inspecting and rewriting the committed image config takes seconds. This is bounded and acceptable for an async operation.
- Docker image tags (`temps-snapshot/<logical_digest>:latest`) accumulate in the local daemon. The final shared-reference `DELETE` also removes the tag. A future storage summary should report daemon image size separately from file-artifact size to give operators a complete picture.
- Cross-backend restore rejection (Docker snapshot → Firecracker backend) is a UX friction point for operators who mix backends. The error message must explain the incompatibility and suggest re-snapping from a sandbox on the target backend.

### Risks

- **Credential leakage into snapshot.** The scrubbing protocol covers known credential paths. Unknown paths (e.g., a user's own `.netrc`, ssh keys written to `/root/.ssh` inside the container by a user script) are not scrubbed. The documentation must make clear that user-written secrets outside the known paths are the user's responsibility to remove before snapshotting. A pre-snapshot warning in the UI ("review that no secrets are stored in the container filesystem before creating a snapshot") is the minimum mitigation.
- **Digest collision.** SHA-256 collision is effectively impossible in practice, but the dedup logic must handle the case where two different content byte streams produce the same digest hash (the test should verify this path returns an error rather than silently corrupting a stored artifact).
- **Partial artifact.** Provider artifacts publish before the database row becomes `ready`. Ordinary post-capture errors mark the row failed and remove the artifact set only after proving that no ready row references its digest. If that reference query fails—or the process crashes in the publication window—the immutable artifact is retained rather than risking deletion of shared data. A grace-period startup reconciler for such crash orphans remains future work.
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

- **Affected crates:** `crates/temps-entities`, `crates/temps-sandbox`, `crates/temps-agents`, and `crates/temps-migrations`.
- **Migration needed:** yes — new `sandbox_snapshots` table and index.
- **Compatibility:** the API and provider trait additions are backward-compatible. Docker snapshots created before this revision lack the immutable image ID and workspace companion metadata required for safe, complete restore. They remain listable and deletable, but restore returns an explicit 422 `LegacyArtifactUnsupported`; users must create a new snapshot from the source sandbox after upgrading. Temps deliberately does not trust a legacy mutable Docker tag or silently restore without the old workspace.
- **Provider boundary:** `temps-sandbox` does not depend on or import `bollard`. The snapshot logic that calls `docker commit` lives in `crates/temps-agents/src/sandbox/docker.rs` behind `SandboxProvider`; the service crate calls trait methods only.
- **Security auditor sign-off required** on the credential scrubbing logic and the test that verifies no known-sensitive env key survives a snapshot commit.
- **Docker integration coverage** extends the provider lifecycle test with take snapshot → delete original sandbox/image tag → import and create from snapshot → verify container-layer and workspace files through exec, gated on Docker availability. Firecracker capture is exercised with real ext4 tools; boot/exec restore remains gated to Linux KVM CI.

## References

- ADR-008 — in-sandbox PTY agent (terminal drop-during-snapshot user impact)
- ADR-009 — sandbox API versioning (optional `from_snapshot` field is non-breaking)
- ADR-010 — provider boundary traits (why snapshot logic lives in `docker.rs` not `temps-sandbox`)
- ADR-013 — sandbox egress credential proxy (threat model for credential scrubbing)
- ADR-029 — Firecracker sandbox backend (persistent disk format and deferred memory-state pause)
- ADR-036 — persistent workspace sandboxes (disk as unbounded resource; workspace lifecycle context)
- `crates/temps-agents/src/sandbox/mod.rs` — `SandboxProvider` trait (object-safety constraint; default-impl pattern)
- `crates/temps-agents/src/sandbox/docker.rs` — reference backend implementation
- `crates/temps-sandbox/src/services/sandbox_service.rs` — `pause_sandbox`/`resume_sandbox` (what snapshots are not)
- `crates/temps-entities/src/sandboxes.rs` — `sandboxes` entity (modelling reference for FK-less project_id)
- Docker: `docker commit` API, `--change` flag for ENV rewrite
- Firecracker: snapshot/restore API — https://github.com/firecracker-microvm/firecracker/blob/main/docs/snapshotting/snapshot-support.md
