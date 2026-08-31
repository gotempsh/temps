# ADR 040: Runtime-synced service template catalog

## Status

Accepted

## Feature maturity

**Beta.** The project, source-revision, redeploy, and security boundaries are
implemented, but community-template compatibility is not yet continuously
qualified across the supported host architectures. Templates that require the
Docker API, host namespaces, devices, privileged mode, or arbitrary host paths
are classified as `host_access` and remain unavailable as project workloads.
They require a separate administrator-controlled server-integration design;
the Beta must not weaken the project sandbox to increase its install count.

## Context

Temps already deploys Docker Compose projects, while Coolify maintains an
Apache-2.0 catalog of hundreds of one-click Compose templates. Copying those
templates by hand would create a permanent maintenance queue and would still
leave three important incompatibilities unresolved:

- Coolify templates use generated `SERVICE_URL_*`, `SERVICE_FQDN_*`,
  credential, and random-value environment variables.
- Some templates request Docker socket access, privileged mode, host
  networking, devices, or fixed host ports. These conflict with Temps' Compose
  security boundary or can collide with another project.
- A mutable upstream Compose file must not silently change an existing Temps
  project on its next deployment.

The catalog is control-plane data, not part of the proxy hot path. The
reference host remains a small 3-vCPU/4-GB instance, so catalog downloads and
response sizes need explicit bounds.

## Decision

Temps reads Coolify's canonical generated catalog from
`https://cdn.coollabs.io/coolify/service-templates-latest.json` on demand.
The backend owns this integration; browsers do not fetch or interpret upstream
Compose directly.

The backend:

1. Downloads at most 4 MiB with five-second connect and twenty-second total
   timeouts.
2. Accepts at most 2,000 catalog entries and caches the last successful result
   for one hour. A failed refresh serves a stale successful snapshot; the first
   failed fetch returns a typed 502 response. Failed refreshes back off for 30
   seconds so concurrent readers do not repeatedly wait on an unavailable
   upstream.
3. Decodes and analyzes each bounded entry once on refresh in a blocking worker
   and caches the results. List requests paginate cached metadata without
   reparsing Compose or blocking the async runtime; a selected template is
   normalized on the detail request.
4. Produces a typed install plan containing every public Compose service,
   routable target port, variable, safety transformation, warning, and required
   capability approval. Existing fixed host bindings become random
   loopback-only bindings; fixed project/container names are removed.
5. Describes Coolify magic variables as typed generators or user inputs. The UI
   generates credentials locally, including dependent Supabase JWT values. Any
   upstream literal credential default is replaced with a generated encrypted
   variable while preserving equality where multiple services shared the same
   bootstrap value. The
   backend plans the final project slug and derives URL/FQDN values through
   Temps' canonical hostname strategy. Project creation claims that exact slug
   or returns 409 so the installer can re-plan once. Sensitive values are
   classified authoritatively by the backend and marked write-only when the
   project is created.
6. Classifies each entry as `standard`, `elevated`, or `blocked`. Database-style
   images and services that initialize writable Docker volumes can request the
   existing limited startup-capability profile, but the user must explicitly
   approve every service. This covers images whose volume seed directory is
   owned by an application UID and therefore needs `DAC_OVERRIDE` during first
   boot. Host socket access, privileged containers, devices, unsafe mounts, and
   unsupported host integration remain blocked.
7. Runs a server-side preflight with the final values before creating a project:
   required variables, architecture metadata, capability approvals, and the
   normalized document must pass `docker compose config --quiet` within a
   bounded timeout. Preflight accepts only declared, size-bounded values, runs
   at most four Compose processes concurrently, and clears the server process
   environment before invoking an absolute Docker CLI path. Values reach
   Compose only through a safely encoded temporary env file.
8. Binds preflight to the SHA-256 digest of the complete install plan returned
   by the detail endpoint: normalized Compose, public route/port metadata, and
   architecture flags. If upstream refreshes between selection and validation,
   preflight returns 409 and requires the user to review the new snapshot.
   Operational Docker, timeout, or filesystem failures return 503; a validly
   executed Compose rejection remains a 200 response with `ready: false` and
   actionable validation errors.

Installing creates a regular Temps-owned `compose` project with the normalized
Compose YAML, environment variables, all public service/port selections,
capability approvals, an informational catalog-origin record, and the normal
Temps deployment pipeline. The initial YAML and every later edit are stored as
immutable Compose source revisions. The Build settings editor can change an
image version or any Compose setting, save a new revision with optimistic
concurrency, and deploy it. Redeploying a historical deployment selects that
deployment's saved revision rather than whichever document happens to be
current.

Project creation optimistically claims the slug planned by preflight. If a
concurrent create wins, it fails safely with 409 and the installer re-runs
preflight once, so collision suffixes, truncation, and multi-route hostnames do
not silently drift. Catalog access requires both project-create and
deployment-create permission so a custom API key is not offered an install it
cannot complete. If a later source-save or deployment step fails, the created
project and its actionable error remain available for inspection and retry.

Updating the remote catalog affects only future installs; an existing project
is independent of Coolify and retains its reviewed revisions. Each deployment
snapshots the source bundle, Compose path, working directory, and discovered
health path needed to replay it after later project edits. Images may still use
mutable registry tags, so identical Compose does not yet guarantee bit-for-bit
reproducible redeployment. The stored provider, slug, catalog revision, and
template timestamp are informational provenance rather than a server-attested
audit record.

The page attributes the catalog to Coolify and links to both the upstream
repository and each service's documentation. Coolify's catalog is distributed
under Apache-2.0; Temps does not claim ownership of the templates or service
logos.

## Security boundaries

The gallery compatibility check is an early user-facing explanation, not the
deployment security boundary. `ComposeExecutor` still performs its complete
policy and filesystem-confinement validation immediately before Docker sees
the document.

The catalog URL is a compile-time constant. Operators cannot turn it into an
arbitrary server-side request target. Response size, entry count, decoded
Compose size, and pagination are bounded. Credentials are generated in the
browser using Web Crypto, submitted once, encrypted at rest by the existing
project environment-variable service, and never returned by the catalog API.

The installer deliberately rejects rather than auto-elevates templates that
request Docker socket access, host namespaces, arbitrary extra capabilities,
devices, or privileged mode. Fixed host ports are safe to normalize because
Temps preserves their container targets while replacing public host exposure
with random loopback bindings. A catalog entry can never approve its own
limited capability profile.

Compose redeployments prepare and pull images while the previous stack remains
live. Candidate plaintext secrets are written under a deployment-scoped
generation, never over the directory mounted by the active containers. A
pre-teardown failure or cancellation deletes only the candidate generation and
preserves the old stack; after the replacement becomes healthy, the candidate
is promoted and obsolete generations are removed. Once teardown has begun,
cancellation performs full compensating stack and secret cleanup. A
process-wide lock keyed by the canonical Temps data directory and Compose
project serializes this complete lifecycle across fresh workflow executor
instances, including superseding and replayed deployments.

## Consequences

- A connected Temps instance receives new compatible Coolify templates without
  a Temps release or catalog maintenance work.
- Opening the gallery performs at most one bounded upstream request per hour
  per Temps process. Normal requests and deployments do not depend on Coolify.
- An offline instance with no successful cache displays a concrete catalog
  connectivity error. It does not hide the Services entry point.
- Multi-public-service templates use the existing `publicPorts[]` route model;
  Temps still supports one public URL per Compose service, so templates that
  require multiple independently routed ports on the same service remain
  blocked with a specific explanation.
- Relative `env_file` entries are supported through the existing deployment
  synthesis behavior. Templates requiring additional build contexts, config
  files, or secret files remain blocked until the catalog integration can copy
  a revision-pinned, path-confined bundle rather than only Compose YAML.
- Upstream compatibility can regress without a Temps code change. Every item is
  therefore analyzed at selection time and the deployer revalidates it at
  installation time.

## Alternatives considered

### Vendor the complete catalog in each release

This is reproducible and works offline, but new services require a Temps
release and the generated catalog adds about one megabyte to every source
update. It does not satisfy the maintenance goal.

### Deploy directly from the Coolify repository

This avoids a catalog cache but clones a large unrelated repository, leaves
Coolify magic variables unresolved, and lets the tracked branch change future
deployments. It was rejected.

### Fetch and run Compose in the browser

This makes availability depend on browser CORS, duplicates parsing logic in
TypeScript, and lets an untrusted remote document skip server-side preflight.
It was rejected.

### Automatically grant requested privileges

This would turn a mutable third-party catalog into authority over the host
Docker daemon. It was rejected; incompatible templates remain visible and
explain which operator-controlled capability is missing.
