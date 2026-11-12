# ADR-097: Internal DNS canary corrections

**Status:** Accepted; pending canary verification

**Date:** 2026-11-08

**Amends:** [ADR-011](./011-internal-dns-ha-databases.md), [ADR-024](./024-control-plane-dns-resolver.md), [ADR-095](./095-internal-dns-observability.md)

Managed standalone services publish the normalized display name, preserving
hyphens. The previously published stored slug remains an A-record alias with
the same owner, address and lifetime. Existing clients keep working; ownership
conflicts still fail through the registry's existing admission checks.

Containers configured with cluster DNS receive only the internal nameserver
and the `temps.local` search domain. Docker's embedded resolver may still appear
in `resolv.conf`; its upstream is the configured internal resolver. External
queries go through the internal resolver's forwarding policy. Host search
suffixes and direct fallback nameservers are not inherited. This takes effect
on container creation; existing containers need recreation. Resolver failure
now fails DNS closed rather than bypassing its policy through host nameservers.

Resolver-coverage reports exclude containers whose injected `TEMPS_NODE_ID`
belongs to another node (the local control plane is node 0). A worker sharing
Docker with a separate local Temps installation must not report that
installation's colliding deployment IDs. Containers without node metadata stay
in the check for compatibility with legacy and Compose workloads. Node IDs are
not globally unique across installations; this does not introduce a general
cross-installation workload identity scheme.

A worker's first failed sync is withheld from the displayed error until a
second consecutive failure. Errors after a successful sync are reported
immediately, and success clears the current error. Failure counters remain
visible. Internal-zone NXDOMAIN responses carry AA, including policy denials;
external-zone denials remain non-authoritative.

Worker DNS authentication failures keep HTTP 401 and include a stable worker
credential code and remediation: verify the node ID, control-plane address and
operator-issued token, and rejoin when revoked. Console sign-in cannot repair
a worker credential.

These are control-plane and container-creation changes except for setting the
AA response bit. Query processing gains no I/O, locks, queues or unbounded
state. Each service adds at most one legacy alias; sync status remains constant
space. No new runtime configuration is introduced.
