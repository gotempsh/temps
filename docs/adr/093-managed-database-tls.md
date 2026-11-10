<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-093: Server-Side TLS for Managed Databases

**Status:** Proposed (Phase 1 implemented)
**Date:** 2026-11-10

Related: [ADR-020](./020-multi-node-deployment-hardening.md) (cluster CA and
node certificates), [ADR-032](./032-postgres-datasource-tls-trust.md) (typed
`disable` / `require` / `verify-full` trust for the data explorer).

## Context

Temps creates and runs PostgreSQL, Redis, MariaDB and MongoDB containers for
its users. Today none of the standalone engines offers TLS:

- **Standalone PostgreSQL** starts without `ssl=on`. The `ssl_mode` service
  parameter is a *client* setting for internal consumers (WAL-G, restore) and
  defaults to `disable`. Linked apps receive a `POSTGRES_URL` with no `sslmode`.
- **HA PostgreSQL** (pg_auto_failover) already runs `--ssl-self-signed` with
  `hostssl` rules for its infrastructure roles, but keeps plain `host` rules for
  apps, precisely because linked apps' URLs carry no `sslmode`.
- **Redis, MariaDB, MongoDB** have no server TLS.

### What is exposed today (threat model, honestly)

- The host port of every managed database binds **127.0.0.1 only**. Nothing
  outside the host can reach it without the operator adding a tunnel or proxy.
- Apps reach a database by container name on the `temps` Docker bridge. Traffic
  between containers on one host never leaves the kernel. An attacker who can
  sniff the bridge already has root on the host, and with it the volumes.
- Traffic between nodes runs over the WireGuard overlay, which is encrypted and
  authenticated.

So TLS adds little for the default single-host deployment. It matters for:

1. **External access**: an operator who exposes a database (SSH tunnel, TCP
   proxy, a public hostname in a later phase) and connects from a laptop or
   another provider.
2. **Compliance**: "encryption in transit everywhere" controls (SOC 2, ISO
   27001, HIPAA, PCI) that auditors check per connection, not per network.
3. **Defence in depth**: a misconfigured overlay or a future transport that is
   not WireGuard.
4. **Verified identity**: `verify-full` lets a client prove it reached the
   database it meant to, not whatever answered on that address.

## Decision

### D1. A dedicated managed-services CA, separate from the cluster CA

Managed database certificates are signed by a **managed-services CA**, never by
the per-cluster CA in `temps_core::node_pki`.

The cluster CA signs worker leaves that carry `serverAuth` *and* `clientAuth`,
and every node and the control plane trust it as the root of cluster identity.
If it also signed database certificates:

- a database's key, which sits in a volume any image running against that
  volume can read, would be a certificate the cluster trusts, and could be
  presented to a node agent or the control plane;
- a worker's leaf would verify as a database server for every client that
  trusts the database CA, so a compromised worker could impersonate any managed
  database to an app that pins the CA.

Two roots keep the two classes of certificate unable to impersonate each other.
Either can be rotated without touching the other.

- ECDSA P-256, valid 10 years, `BasicConstraints: CA, pathlen 0`: it can sign
  only leaves, never another CA.
- Stored in `managed_service_certificate_authorities` (one row per `purpose`;
  today only `managed-services`). The key is encrypted at rest with
  `EncryptionService` and never leaves the control plane, a response or an
  audit payload.
- **Created lazily**, the first time a service needs a certificate. A unique
  index on `purpose` plus `INSERT … ON CONFLICT DO NOTHING` and a read-back
  makes concurrent first use converge on one CA.

### D2. One server certificate per service

- ECDSA P-256 leaf, valid 365 days, `serverAuth` only (never `clientAuth`), a
  random 128-bit serial, back-dated 5 minutes for clock skew.
- **SANs** are server-authoritative, never client-supplied:
  - the container name on the `temps` network (`postgres-<name>`): the strong
    identity apps verify;
  - the overlay name `<service>.temps.local`, when the name yields a DNS label;
  - `localhost` and `127.0.0.1`, for host clients of the loopback-bound port;
  - a public hostname, once external exposure exists (later phase).

  Loopback SANs are shared by every service's certificate. `verify-full` against
  `localhost` therefore only proves "some Temps-managed database"; the port
  tells them apart. Clients that need identity verify the container or overlay
  name.
- Stored in `external_service_tls` with the key encrypted at rest. The
  certificate is reused until it is inside the renewal window (30 days before
  `notAfter`), its required SANs change, or the CA changes. Issuance runs under
  a row lock (`SELECT … FOR UPDATE`), so two concurrent lifecycle operations
  cannot each deliver a different certificate.
- **Delivery into the service's volume**: a helper container that mounts the
  data volume is *created and never started*. Each install names its helper
  uniquely and removes only the container it created, by ID, so it can never
  touch an operator's container or a concurrent install's helper. Docker's archive API reads the
  image's own `/etc/passwd` to resolve the engine user's uid/gid (999 in the
  Debian PostgreSQL images, other values elsewhere), then uploads a tar whose
  headers set owner and mode: directory `0700`, certificate `0644`, key `0600`.
  PostgreSQL refuses a group- or world-readable key it does not own as root, so
  the mode is load-bearing. No image code ever runs against the data to fix
  permissions. Files live outside PGDATA (`/var/lib/postgresql/tls/`), so base
  backups, restores and major upgrades never copy or replace them.
- The material reaches the engine through `ServiceConfig.parameters`
  (`server_tls_mode`, `server_tls_material`) only on container lifecycle paths.
  Engines never serialize those keys back, the parameter strategy rejects them
  from clients at creation and update, and `Debug` prints only the fingerprint.
- Missing keys mean "not said", never `off`. Engine paths that rebuild a
  container from the stored parameters, such as a backup recreating it to turn
  WAL archiving on, or an in-place restore removing and recreating it, keep
  the TLS the replaced container served. Its certificate stays on the data
  volume outside PGDATA, which both reuse. Start-time drift repair leaves such
  a container's TLS alone.
- A major upgrade starts the new server with the service's certificate
  installed in the new data volume, so clients that require TLS keep
  connecting.

#### HA PostgreSQL clusters

A cluster gets **one certificate for all its data nodes**, never for the
monitor, which apps do not reach. The data nodes hold the same data and already
trust each other, and whichever one is primary must match the VIP apps connect
to. Its SANs:

- the VIP `<service>.temps.local` in linked apps' `POSTGRES_URL`;
- each data node's container name, advertised hostname (an overlay FQDN or a
  private address) and overlay IP;
- loopback.

Adding a member changes the names, so the certificate is re-issued and applied
to every data node.

pg_auto_failover already runs every data node with `--ssl-self-signed`, and
pg_autoctl keeps `sslmode=require` on its own connections to the monitor and
its peers. Managed TLS therefore only swaps the certificate a data node
serves. It is applied **in place, on the running member, with no restart**,
over the member exec used by the SCRAM upgrade: `docker exec` locally, the
node agent's exec on a worker. The steps:

1. The certificate and key travel in the exec environment, never in the
   container's configuration or command line.
2. They are written to `/var/lib/postgresql/tls/` on the member's volume,
   outside PGDATA, so a standby's base backup never copies them: directory
   `0700`, key `0600`, both owned by `postgres`.
3. A pair whose public keys differ is refused before anything is switched.
4. The paths are recorded with `pg_autoctl config set ssl.cert_file /
   ssl.key_file`. That writes pg_autoctl's own configuration on the volume,
   which pg_autoctl turns into `ssl_cert_file` / `ssl_key_file` at every start,
   so the setting survives restarts.
5. pg_autoctl reloads PostgreSQL, which loads a new TLS context on SIGHUP. The
   reload is repeated until both settings landed, because a second
   `config set` arriving during pg_autoctl's own reload can be missed.
6. The script reads the certificate the member actually serves
   (`openssl s_client -starttls postgres`) and reports success only when its
   fingerprint matches.

Each script runs under `timeout` inside the member, which kills its whole
process group before the control plane stops waiting. A Docker exec cannot be
killed from outside, so when the control plane gives up on a member, nothing
is left running there. A mode change holds a per-service lock from reading the
current mode through its rollback, and every apply pass takes the same lock and
reads the setting under it, so no older change or pass lands after a newer one.

`off` points pg_autoctl back at its self-signed pair in PGDATA and removes the
managed files. HBA rules are never touched, so plaintext stays accepted in
both modes, exactly as before.

### D3. A per-service TLS setting on the service's record

The setting is per service, stored in `external_service_tls.mode`, changed
through the API and audit-logged (`EXTERNAL_SERVICE_TLS_MODE_CHANGED`, with the
previous and new modes). It is never an environment variable: two services on
one instance can be in different modes.

| Mode | Server | Plaintext | Who is encrypted |
|---|---|---|---|
| `off` | no TLS (a cluster's data nodes: pg_auto_failover's self-signed certificate, which no client can verify) | accepted | nobody (cluster: clients that ask, unverified) |
| `available` | TLS offered | accepted | clients that ask (`sslmode=prefer`/`require`/`verify-full`, `rediss://`, `tls=true`) |
| `required` | TLS offered | **rejected** (`hostnossl all all all reject`, Redis `port 0` + `tls-port`, MariaDB `require_secure_transport=ON`, MongoDB `requireTLS`) | everyone |

- **No row means `off`.** Every service that exists when this ships keeps exactly
  today's behaviour, with no backfill.
- **New standalone PostgreSQL services and new PostgreSQL clusters default to
  `available`.** Nothing that connects today breaks: plaintext is still
  accepted. libpq's default `prefer` (psql, most libpq-based drivers) starts
  encrypting immediately. Standalone services on worker nodes, imported
  containers and the other engines default to `off` until their phase lands.
- `available` encrypts only clients that ask. That limit is stated wherever
  the setting is shown.
- Changing the mode re-initializes a running standalone service through the same
  path a parameter update takes (stop, init, start with drift repair). The
  container is recreated through the recreate guard, so **it restarts the
  database**. A running cluster's data nodes switch certificates in place with
  no restart. A stopped service only records the mode; its next start applies
  it. If applying fails, the previous mode is restored (and re-applied to the
  cluster members that had switched) so the record keeps describing what runs.
- A cluster is converged on its setting at every point where the cluster code
  already converges authentication:
  - control-plane start, which also renews a certificate inside its renewal
    window;
  - the end of cluster creation;
  - after a member is added;
  - after a cluster Start.

  This is best-effort. A data node that is stopped, unreachable or still taking
  its first base backup is listed as pending in the status and retried at the
  next of those points.

### D4. Per-engine server configuration and hot reload

| Engine | Enable | Reload a renewed certificate without a restart |
|---|---|---|
| PostgreSQL | `-c ssl=on -c ssl_cert_file=… -c ssl_key_file=…` | rewrite files, `SELECT pg_reload_conf()` |
| Redis | `--tls-port 6379 --port 0\|6380 --tls-cert-file … --tls-key-file … --tls-auth-clients no` | `CONFIG SET tls-cert-file …` / `tls-key-file …` |
| MariaDB | `--ssl-cert … --ssl-key …` | `FLUSH SSL` (10.4+) |
| MongoDB | `--tlsMode allowTLS\|requireTLS --tlsCertificateKeyFile …` | `db.adminCommand({rotateCertificates: 1})` (5.0+) |

A standalone container records the fingerprint of the certificate it was
created with (label `temps.server_tls_fingerprint`); a cluster's data nodes are
inspected by the certificate they serve. Start-time drift detection recreates a
container whose TLS state disagrees with the record: TLS on but should be off,
off but should be on, or on with a different certificate. Phase 1 applies a
renewed certificate by recreating the container. Phase 2 switches renewal to the
hot-reload column above, so rotation costs no downtime.

A lifecycle path that does not carry the material must never strip TLS from a
container. The desired state is "unknown" there and drift is not evaluated. If
such a path has to create a fresh container, it starts without TLS rather than
pointing the server at files that may not exist. Plaintext is accepted in every
mode that ships, and the status API reports the container as not offering TLS.

### D5. Client trust distribution (Phase 2)

Applied only when the operator opts in per link or sets `required`, so existing
apps never break silently:

- The CA certificate is mounted read-only into linked app containers at
  `/etc/temps/managed-ca.pem`.
- URL options: PostgreSQL `?sslmode=verify-full&sslrootcert=/etc/temps/managed-ca.pem`,
  Redis `rediss://`, MongoDB `tls=true&tlsCAFile=…`, MariaDB `ssl-mode=VERIFY_IDENTITY`
  plus `ssl-ca`.
- A `<ENGINE>_CA_CERT` variable (`POSTGRES_CA_CERT`, …) with the PEM itself, for
  drivers that ignore URL options (node-postgres, several ORMs) and for platforms
  where a mounted file is awkward.
- The CA download endpoint and its SHA-256 fingerprint let an operator configure
  external clients and check the file out of band.

### D6. Every internal consumer honours the setting

Before `required` can be offered, every internal consumer connects with TLS and
**has no plaintext fallback when TLS is required**: pg_dump and WAL-G backups,
restore and point-in-time restore, the metrics collector, the cluster role
reconciler, health probes, major upgrades (`pg_upgrade` helpers) and the data
explorer. The data explorer follows [ADR-032](./032-postgres-datasource-tls-trust.md):
typed `disable` / `require` / `verify-full`, verifying against the
managed-services CA. In `available` mode today's behaviour is unchanged. The
explorer's TLS ladder already falls back from verified TLS to unverified TLS on
private addresses, and admin SQL and backups use `sslmode=disable` and keep
working.

### D7. Renewal

- A control-plane job finds certificates whose `not_after` is within 30 days,
  using the partial index on `external_service_tls(not_after)`. The cost is
  proportional to the certificates due, not to the total number of services.
  It runs daily and staggers re-issuance so that one bad day cannot expire
  everything at once.
- Re-issue, deliver, hot-reload (D4). Recreate only for an engine without hot
  reload. A failed renewal is logged at `warn!`, retried with backoff and
  surfaced in the status (`not_after`, days left) long before expiry.
- CA rotation (10-year CA, so rare): add the new CA, cross-trust during an
  overlap (clients get both CAs in their bundle), re-issue every leaf, then
  retire the old CA. The `purpose` and `authority_id` columns already allow two
  CAs and record which one signed each leaf.

### D8. Surface and onboarding

- The PostgreSQL service page shows a **Server TLS** card on every supported
  service, even when off. It shows:
  - the mode;
  - whether the running container offers it, and for a cluster which
    certificate each data node serves;
  - a pending notice;
  - certificate validity and SANs;
  - the CA fingerprint, with a download button.

  It states plainly that `available` encrypts only clients that request TLS,
  with a concrete `sslmode=require` example.
- `GET /external-services/{id}/tls` returns `supported` plus `unsupported_reason`,
  so a client can tell "not available for this service yet" from "off".
- Enabling or disabling asks for confirmation. The confirmation says whether it
  restarts the database (standalone) or switches certificates in place
  (cluster); the status carries `change_restarts_database` so a client can tell
  which.

## Phases

1. **PostgreSQL standalone and HA clusters, `available`** (this change):
   - CA, per-service certificate, `external_service_tls` migration;
   - delivery and `ssl=on` with start-time drift repair (standalone);
   - in-place certificate swap on every data node (clusters, on any node);
   - status, mode change and CA download APIs, and the console card.

   New standalone PostgreSQL services and new clusters default to `available`.
   `required` is rejected as not yet supported. Linked apps' `POSTGRES_URL` is
   unchanged. TLS is kept through backups, in-place restores and major
   upgrades. A service created by restoring into a new service starts `off`.
   Temps' own connections encrypt but do not verify the managed CA yet.
2. **PostgreSQL `required`**: client trust distribution (D5); every internal
   consumer (D6); `hostnossl … reject` in pg_hba; the renewal job with hot
   reload (D7); restore-to-new-service carries the TLS state.
3. **Redis, MariaDB, MongoDB**: the same model, with the engine-specific settings
   and reloads in D4. Standalone services on worker nodes: the agent receives the
   material over the existing mTLS channel (as cluster members already do
   through the agent's exec), and the SANs add the node's overlay address. A
   cluster restored in place re-creates its data nodes' pg_autoctl state, so
   restore re-applies the setting directly instead of waiting for the next
   start.

## Consequences

- **Load and memory**: control plane only. Issuing a certificate is a few
  milliseconds of ECDSA work, done once per service per year (or on a name
  change), plus one small row. Delivery creates one never-started helper
  container per (re)creation. Nothing is added to the proxy, ingest or any
  per-request path. Inside the database, TLS costs the handshake per
  connection and a few percent CPU for encrypted traffic, only for clients that
  ask for it.
- Every new PostgreSQL service is created with a certificate and an extra
  helper-container step. A failure there fails creation with an error naming the
  service and volume, rather than silently creating a database without TLS.
- Toggling the mode restarts a standalone database; the UI says so before it
  happens. On a cluster it costs one exec per data node and a reload, with no
  restart and no failover.
- Reading a cluster's TLS status execs one read-only `openssl s_client` per data
  node, concurrently, each bounded to 15 s. The status is read when an operator
  opens the service page, not polled by the control plane.
- `down` of the migration loses the CA and the issued certificates. Containers
  keep serving the files already in their volumes until they are recreated by
  the previous binary, which does not set `ssl=on`.

## Alternatives considered

- **Reuse the cluster CA**: rejected (D1).
- **A self-signed certificate per service** (what pg_auto_failover's
  `--ssl-self-signed` does): encrypts, but no client can verify it, so
  `verify-full` is impossible, and every service would have its own trust root
  to distribute.
- **Let the image generate its own certificate at first boot**: same
  verification problem, and the key would never be under Temps' renewal control.
- **A process-wide `TEMPS_DATABASE_TLS` switch**: one value per instance, not
  per database, not audited, needs a restart. Rejected by the
  configuration-is-a-column rule.
- **A TLS-terminating sidecar per database**: works for every engine
  uniformly, but costs a process and memory per database on 2 GB hosts, and the
  database still sees every client as the sidecar.
- **Fix key permissions by running a shell in the image** (`chown`/`chmod` in a
  started helper): executes image code against the data volume, which the
  WAL-G probe already avoids. The archive API sets owner and mode without
  executing anything.
