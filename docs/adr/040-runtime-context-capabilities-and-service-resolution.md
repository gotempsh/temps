<!-- SCOPE: Defines how Temps models its execution environment, runtime capabilities, workload backends, and control-plane service endpoints. -->

# ADR-040: Runtime Context, Capabilities, and Service Resolution

**Status:** Accepted
**Date:** 2026-08-29
**Author:** Temps Contributors

## Context

Temps can be installed as a native process managed by systemd or launchd, and
can also be packaged as a Docker container. Kubernetes-hosted operation is a
future target. These environments do not share the same networking or host
capabilities:

| Concern | Native host | Docker container | Kubernetes pod |
| --- | --- | --- | --- |
| Control-plane dependency address | Loopback + published host port | Compose service DNS + container port | Kubernetes Service DNS + Service port |
| Internal listener | Loopback where practical | Pod/container interface | Pod interface |
| External exposure | Host firewall or reverse proxy | Explicit host publishing | Service, Gateway, or Ingress |
| Docker API | Local socket when configured | Mounted socket or remote API | Usually unavailable |
| KVM / Firecracker | Possible on Linux with `/dev/kvm` | Not available by default | Not available by default |
| Host service manager | systemd or launchd | None | None |
| Host network administration | Possible with explicit privilege | Namespaced and restricted | CNI-owned and restricted |

The current `temps_core::DeploymentMode` partially recognizes this problem. It
has `Baremetal` and `Docker` variants and chooses either `127.0.0.1` with a host
port or a container name with an internal port. However:

1. It reads the ambient `DEPLOYMENT_MODE` environment variable at call sites
   instead of receiving immutable startup configuration.
2. Unknown values, including `kubernetes`, silently become `Baremetal`.
3. Mode checks are spread through providers, routes, readiness checks, and
   deployment jobs.
4. One enum is being asked to describe both network location and available
   execution features.
5. Tests mutate process-global environment state and need global mutexes.
6. Packaging can be inconsistent with the selected mode. For example, a
   Compose manifest can provide internal service names but omit Docker mode,
   causing code to generate loopback addresses from inside the Temps container.

This is not only an addressing issue. A Docker-packaged Temps instance can
manage sibling Docker containers when the Docker socket and networks are
available, but it cannot safely assume access to KVM, host firewall rules,
systemd, launchd, arbitrary bind mounts, or host network namespaces. A
Kubernetes-hosted instance has a different API and identity model again.

We therefore need an explicit architecture boundary before treating Docker or
Kubernetes as first-class ways to run the Temps control plane.

## Decision

Temps will separate four concepts that are currently conflated:

| Concept | Answers | Examples |
| --- | --- | --- |
| Execution environment | Where is the Temps control-plane process running? | `Host`, `Docker`, `Kubernetes` |
| Service endpoint resolver | How does this process reach a named dependency? | Loopback, Compose DNS, Kubernetes Service DNS |
| Workload backend | What executes a user workload or sandbox? | Docker, Firecracker, future Kubernetes |
| Capability registry | What can this specific installation actually do? | Docker API, KVM, host networking, Kubernetes API |

The execution environment selects default adapters at the composition root. It
does not become a feature flag checked throughout business logic.

### Delivery boundary for the current implementation

The first implementation of this ADR includes only `Host` and `Docker` runtime
adapters. Kubernetes appears in this document to constrain the interfaces and
prevent Docker-only assumptions from leaking into business logic, but this
iteration does not add Kubernetes crates, manifests, API clients, RBAC,
resolvers, workload providers, or integration tests.

The architectural target is complete when a future Kubernetes adapter can be
added at the composition root without changing endpoint consumers or domain
services. It is not necessary to ship an unused Kubernetes implementation
today. The Docker delivery implements the immutable Host/Docker context and
typed endpoint values first; capability probing and the remaining compatibility
call-site migration stay explicitly tracked in the migration plan below.

### 1. Construct one immutable runtime context at startup

The CLI composition root constructs and validates one `RuntimeContext`, then
injects it into routing and readiness services. In the initial Docker delivery
it contains the validated environment, resolver, and configuration source. The
completed architecture will additionally contain:

- A validated `ExecutionEnvironment`: `Host`, `Docker`, or `Kubernetes`.
- A `ServiceEndpointResolver` selected for that environment.
- A capability snapshot with availability, reason, and remediation.

Workload providers are constructed at the same composition root but remain
separately registered through their existing domain traits. `RuntimeContext`
does not become a provider locator, and consumers continue to receive
`Arc<dyn SandboxProvider>` directly as required by ADR-010.

Runtime configuration MUST be parsed once. Application services MUST NOT read
`DEPLOYMENT_MODE` or a replacement environment variable during requests or
jobs. Tests will construct a context directly instead of mutating process-global
environment variables.

`ExecutionEnvironment` describes location only. It MUST NOT imply that Docker,
Firecracker, Kubernetes, or privileged host operations are available.

### 2. Make execution environment explicit and fail closed

Installers and manifests MUST set the execution environment explicitly:

| Packaging | Required value | Owner |
| --- | --- | --- |
| systemd / launchd | `host` | `deploy.sh` or generated service unit |
| Docker Compose | `docker` | Compose manifest |
| Kubernetes (future) | `kubernetes` | Helm chart or manifests |

An absent value MAY temporarily default to `host` for backward compatibility.
At the end of the migration, an unknown value MUST fail startup with a typed
configuration error; it MUST NOT silently fall back to host addressing.

During migration, the canonical `TEMPS_EXECUTION_ENV` input takes precedence
over legacy `DEPLOYMENT_MODE`. Values `host` and `baremetal` map to `Host`,
`docker` maps to `Docker`, and unset temporarily maps to `Host`. The reserved
value `kubernetes` returns a typed "not supported by this build" startup error
until the Kubernetes adapter ships. Other unknown values return a typed invalid
configuration error instead of falling back. Release notes MUST call out this
change from the legacy silent host fallback.

Automatic detection MAY produce a diagnostic suggestion, but MUST NOT silently
override explicit configuration. Container heuristics such as `/.dockerenv` or
cgroup contents are not a reliable configuration contract.

### 3. Resolve service identity separately from network location

Consumers will request an endpoint using a stable service identity and logical
port instead of assembling a hostname from the execution environment.

At the composition and packaging boundary, resolution follows this precedence:

1. An explicit operator-provided endpoint, such as `TEMPS_DATABASE_URL` or
   `TEMPS_CLICKHOUSE_URL`.
2. The environment-specific resolver configured at startup. The initial
   Host/Docker resolver implements this defaulting layer; existing explicit URL
   consumers remain authoritative while their call sites are migrated.
3. A typed `EndpointUnavailable` error containing the service identity,
   execution environment, and missing configuration.

Resolvers use these defaults:

| Environment | Hostname | Port |
| --- | --- | --- |
| Host | `127.0.0.1` | Published host port |
| Docker Compose | Stable Compose service name | Container target port |
| Kubernetes | Stable Kubernetes Service DNS name | Service port |

Docker resolution MUST use Compose service names or explicit network aliases,
not generated container IDs and not host-published ports. Kubernetes resolution
MUST use Services, not Pod names or Pod IPs. A fully qualified Kubernetes name
MAY be used where cross-namespace resolution is required.

Returned endpoints will be typed values containing scheme, host, port, and
optional TLS/server-name metadata. Business logic MUST NOT perform string
replacement on connection URLs to adapt them to a runtime.

This resolver covers dependencies of the Temps control plane and the
orchestrator endpoint used to reach a workload after its backend has created
it. It explicitly does not own `.temps.local`, `service_endpoints`, user-managed
service identity, or multi-node DNS records. ADR-011 and ADR-024 remain
authoritative for those concerns.

### 4. Separate listening from publishing

A listener address and an externally published address are different contracts.

| Environment | Process listener | Internal dependencies | External access |
| --- | --- | --- | --- |
| Host | `127.0.0.1` unless direct ingress is explicitly selected | Loopback | Reverse proxy/firewall policy |
| Docker | Explicit private interface address | Private control endpoint; no publication required | `127.0.0.1:host:container` by default |
| Kubernetes | `0.0.0.0` inside the pod network namespace | ClusterIP Service DNS | Gateway/Ingress or explicit Service type |

Binding a process to `127.0.0.1` inside a container or pod would make it
unreachable through the container network. The Compose Temps process therefore
binds to its explicit private control-network address, not `0.0.0.0` and not
its workload-network interface. Third-party data-service images may listen on
all interfaces inside their isolated network namespace, but their host
publication remains loopback-only.

Compose MUST bind any published diagnostics or control-plane ports to
`127.0.0.1`. PostgreSQL and ClickHouse do not need host publication for Temps
itself; publication is an operator convenience and SHOULD be omitted by default
or placed behind an explicit diagnostics profile. Kubernetes data services MUST
default to `ClusterIP`, never `NodePort` or `LoadBalancer`.

### 5. Select workload backends through provider boundaries

Execution environment and workload backend are independent axes:

- A native Linux installation can use Docker and Firecracker together.
- A Docker-packaged control plane can use the Docker API when its socket and
  required networks are mounted.
- A future Kubernetes-packaged control plane can use a Kubernetes workload
  provider when its service account and RBAC allow it.
- A backend requested by a user MUST fail with a descriptive unavailable error
  when its capabilities are missing; it MUST NOT silently downgrade.

Existing provider traits remain the boundary. This ADR does not introduce one
large runtime trait that contains every host operation. Docker, Firecracker,
and future Kubernetes implementations remain domain-specific providers and are
assembled according to the validated context.

### 6. Represent support as probed capabilities

Temps will expose a capability registry rather than infer support from the
execution environment alone. Each capability reports `available`, an optional
version, a reason when unavailable, and an actionable setup path.

Initial capabilities include:

| Capability | Required evidence |
| --- | --- |
| Docker workload execution | Docker API ping plus required network access |
| Docker host management | Socket/API access and required labels/permissions |
| Firecracker sandbox execution | Linux, KVM access, provisioned binaries/kernel, Docker OCI image pipeline, networking smoke test |
| Kubernetes workload execution | API discovery, namespace access, and required RBAC |
| Host service management | Supported systemd or launchd control path |
| Host network administration | Explicit privilege and successful read-only preflight |
| Host filesystem integration | Required paths mounted and writable with expected ownership |

Capabilities are probed at startup and refreshed through explicit health,
reconciliation, and pre-operation probes. Feature surfaces remain visible when
unavailable and return the reason and setup path, following the repository
onboarding rule.

Capability-sensitive operations MUST gate at the service boundary and return a
typed unavailable or permission error before creating partial infrastructure.
Startup results are not permanent truth: Docker credentials, socket access,
Kubernetes tokens, RBAC, and devices can change. Providers MUST re-probe before
privileged mutations when the cached result is stale or after an authentication,
authorization, transport, or device error. A successful re-probe updates the
registry; a failed one records the reason and remediation.

### 7. Treat Firecracker in containers as unsupported by default

Firecracker requires more than `/dev/kvm`: it needs verified binaries and guest
artifacts, jailer-compatible filesystem ownership, TAP/bridge configuration,
cgroup control, and host network privileges. Passing these wholesale into the
Temps control-plane container would substantially weaken the container boundary.

Therefore:

- Native Linux with successful Firecracker preflight is the supported initial
  Firecracker environment.
- The standard Compose and Kubernetes packages report Firecracker unavailable
  with a concrete explanation.
- A future privileged worker/agent design MAY provide Firecracker to a
  containerized control plane without granting those privileges to the
  control-plane container itself.
- Merely mounting `/dev/kvm` MUST NOT make the capability appear available.

This preserves ADR-029's provider model while being honest about packaging.

### 8. Packaging owns environment-specific wiring

Each packaging layer is responsible for creating the network objects and
configuration its resolver expects:

| Packaging | Responsibilities |
| --- | --- |
| `deploy.sh` | Install native service, publish managed dependencies on loopback, configure host endpoints, report host capabilities |
| Docker Compose | Set Docker execution environment, join stable networks, configure environment-specific endpoints, mount only required host resources |
| Kubernetes manifests/Helm | Set Kubernetes execution environment, create Services, Secrets, RBAC, storage, probes, and ingress objects |

The Compose stack MUST use configurable static addresses on its private
control network for PostgreSQL and ClickHouse, regardless of whether those
services also have loopback host mappings. Private dependency addresses MUST
NOT depend on Docker DNS: a workload can claim an identical service or network
alias on the second network of a dual-homed Temps container. Temps also joins
an explicitly named workload network matching `TEMPS_NETWORK_NAME`;
PostgreSQL and ClickHouse MUST NOT join that workload network. A future
Kubernetes package will use authenticated Services such as
`temps-postgres.<namespace>.svc` and `temps-clickhouse.<namespace>.svc`.

The private control network MUST be an internal Docker network. Static
addressing prevents DNS alias confusion; the internal-network boundary prevents
workloads from routing directly to those addresses across Docker bridges.

Because the Compose control plane is attached to both networks, its HTTP, TLS,
and console/admin listeners MUST bind only to its configured static private
control-network address, never `0.0.0.0`, a DNS-derived address, or the
workload-network address. Public HTTP, TLS, and ingest listeners are forwarded
through an unprivileged public ingress container, published only on
`127.0.0.1`, and attached to the workload network. Managed workloads can reach
this deliberately public surface, so it MUST return 404 for admin,
authentication, UI, and management routes.

The admin/UI listener binds to a separate private control address and port. It
is published on host loopback through a physically separate HTTP reverse proxy
on a bridge that does not join or advertise itself on the workload network and
has an independent, strong, randomly generated Basic-auth password mounted as a
file secret. Because Docker engines differ in whether they route a numerically
addressed packet across otherwise-unconnected bridges, network separation is
defense in depth and the authentication barrier MUST remain fail-closed. The
proxy MUST bcrypt the secret at startup, rate-limit authentication failures,
strip the Basic `Authorization` header before forwarding, and preserve Temps'
own authentication and authorization checks. It MUST run non-root with a
read-only root filesystem, all Linux capabilities dropped, no-new-privileges,
bounded memory and PIDs, and no Docker socket or host mounts. Public TCP
forwarding MUST use an event-driven proxy with per-source connection ceilings
and idle timeouts; process-per-connection forwarding is not permitted.
Extending admin access beyond loopback or a secure tunnel requires TLS or mTLS.
Because Basic auth consumes the `Authorization` header, bearer-token admin
clients require a deliberately separate trusted or mTLS ingress path.

The unused global Redis container is not a control-plane dependency and is not
part of this runtime contract. Redis used by the optional KV feature continues
through its managed-service provider and capability path.

Mounting the Docker socket gives the Temps process root-equivalent authority on
the Docker host; running the process as a non-root container user does not
remove that authority. Compose documentation and capability reporting MUST make
this explicit. The Kubernetes package MUST NOT mount a node Docker/containerd
socket into the control-plane pod; Kubernetes operations use a scoped service
account and audited RBAC instead.

## Migration plan

1. **Introduce typed values.** Add `ExecutionEnvironment`, `RuntimeCapability`,
   capability status, and typed service endpoint values without changing
   behavior.
2. **Build runtime context once.** Parse configuration in the CLI composition
   root, register the immutable context, and keep `DEPLOYMENT_MODE` as a
   deprecated compatibility input.
3. **Add resolvers.** Implement host and Docker resolvers. Explicit URLs remain
   highest priority. Keep the resolver contract free of Docker-specific types;
   do not add a Kubernetes resolver in this iteration.
4. **Migrate call sites.** Replace `DeploymentMode::current`,
   `is_docker`, `is_baremetal`, `get_effective_host_port`, and
   `build_container_url` incrementally. Remove mode checks from business
   services as provider injection reaches them.
5. **Correct packaging.** Set the environment explicitly in `deploy.sh` and
   Compose, use internal Docker endpoints, and stop publishing data ports by
   default.
6. **Remove the compatibility helper.** Unknown values become startup errors;
   delete ambient environment reads and their global test mutexes.
7. **Add Kubernetes packaging only after its provider, RBAC, storage, routing,
   upgrade, and rollback contracts pass the common conformance suite.**

## Verification

The runtime boundary requires a conformance matrix, not only unit tests:

| Test | Host | Docker Compose | Kubernetes |
| --- | --- | --- | --- |
| Resolver returns expected dependency endpoint | Required | Required | Required before support |
| Internal service works without public data port | Required | Required | Required before support |
| Invalid environment fails with typed error | Required | Required | Required |
| Capability reason and remediation are accurate | Required | Required | Required |
| Docker workload lifecycle | When Docker exists | Required | Provider-dependent |
| Firecracker lifecycle | KVM runner | Must report unavailable | Must report unavailable initially |
| Control-plane readiness and migrations | Required | Required | Required before support |

Docker integration tests MUST inspect the rendered Compose configuration and
exercise service-to-service connectivity after removing host mappings for data
services. Kubernetes tests SHOULD use an ephemeral real cluster such as `kind`
and MUST verify Service DNS, RBAC denial paths, persistent storage, and ingress.
Docker- and KVM-dependent tests skip gracefully at runtime when their required
backend is unavailable.

## Consequences

### Positive

- Network addresses are correct by construction for host, Docker, and future
  Kubernetes execution.
- Features report honest availability rather than failing deep inside a job.
- Firecracker remains a supported host capability without forcing the entire
  control plane to be native forever.
- Business logic becomes independent from ambient environment variables and
  container-name conventions.
- Internal databases no longer need public host exposure for container or
  Kubernetes operation.
- Runtime behavior can be tested with small injected contexts and shared
  backend conformance suites.

### Negative

- The existing two-variant `DeploymentMode` helper must be migrated across
  several crates.
- Packaging gains an explicit compatibility contract with the binary; changing
  a service name or Service port becomes a reviewed deployment change.
- Capability probing and reporting add startup and diagnostics surface area.
- Kubernetes support still requires a real workload provider and operations
  work; this abstraction makes it possible but does not make it free.
- Some features will be visibly unavailable in the standard Docker package,
  particularly Firecracker and host service/network management.

## Alternatives considered

- **Keep extending the global `DeploymentMode` enum and branch everywhere.**
  Rejected because it couples unrelated capabilities to network location,
  spreads conditionals, and retains global-state tests.
- **Automatically detect the environment.** Rejected as the source of truth;
  nested containers, Docker Desktop, Kubernetes runtimes, and host-mounted
  sockets make detection ambiguous. Detection remains diagnostic only.
- **Always connect through host-published ports.** Rejected because it adds NAT,
  requires unnecessary exposure, and does not translate to Kubernetes.
- **Bind every process to `127.0.0.1`.** Rejected because loopback is scoped to
  each network namespace and would break container-to-container and
  pod-to-Service traffic.
- **Run the Docker package fully privileged to preserve all native features.**
  Rejected because the convenience would erase the security boundary and make
  compromise of the control plane equivalent to unrestricted host compromise.
- **Hide unavailable features based on execution environment.** Rejected because
  environment is only a hint and hidden features violate Temps' onboarding
  principle. Capabilities are shown with reasons and remediation.
- **Create a single universal infrastructure provider.** Rejected because it
  would become a large, unstable abstraction. Endpoint resolution, sandbox
  execution, deployments, and host administration keep separate contracts.

## Scope and related decisions

This ADR defines the runtime composition and addressing contract. It does not
declare Kubernetes a currently supported Temps deployment target, implement a
Kubernetes workload provider, or replace the multi-node DNS architecture.

- ADR-010 defines provider boundaries and remains authoritative for backend
  abstraction rules.
- ADR-011 defines internal DNS for multi-node managed databases.
- ADR-029 defines the Firecracker sandbox backend and its host requirements.
- ADR-007 concerns user projects deployed from Docker Compose; it is separate
  from packaging the Temps control plane with Docker Compose.

---

**Maintenance:** Review when adding a control-plane execution environment, a
workload provider, a new control-plane data service, or changing installer,
Compose, Kubernetes, Firecracker, or internal DNS behavior. Owner: Temps
infrastructure maintainers. Last reviewed: 2026-08-29.
