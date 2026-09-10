<!-- SCOPE: PostgreSQL data-explorer connection security and per-service TLS trust policy. -->

# ADR 032: PostgreSQL Datasource TLS Trust and Downgrade Prevention

- **Status:** Proposed
- **Date:** 2026-07-13
- **Deciders:** David
- **Related finding:** Security finding #9 — PostgreSQL data-explorer TLS failure silently falls back to plaintext
- **Related crates:** `temps-query-postgres`, `temps-providers`
- **Security review required:** Yes. This decision controls whether database credentials and query results may cross the network without authenticated encryption.

---

## Context

The PostgreSQL data explorer creates two connections for a service: an administrative connection used to provision the read-only `temps_explorer` role and a second connection used for browsing and queries. Both paths call `PostgresSource::connect` without a transport-security policy.

`PostgresSource::connect` currently:

1. attempts TLS with a verifier that accepts every certificate, including self-signed certificates;
2. logs a warning if the TLS attempt fails; and
3. retries the same credentials over a plaintext connection.

This is a downgrade vulnerability. An active network attacker can make the TLS handshake fail and cause credentials, queries, and results to be sent without encryption. A successful TLS connection is also unauthenticated because every certificate is accepted, so it protects against passive observation but not server impersonation.

The provider configuration already contains an updateable `ssl_mode` field in `PostgresInputConfig` and `PostgresConfig`. Service parameters, including the password and this field, are serialized into the encrypted `external_services.config` value. The query service deserializes that configuration but currently drops `ssl_mode` when it calls `PostgresSource::connect`.

The existing field is therefore the correct per-service control. A new database column or environment variable would duplicate the model and violate the project rule that runtime configuration belongs on the relevant record. No schema migration is required because the full parameter document is already encrypted at rest.

### Requirements

The design must satisfy all of the following:

- A failed TLS handshake must never change the selected transport policy.
- Operators must be able to connect to a plaintext local PostgreSQL instance intentionally.
- Operators must be able to require encryption from a server with a self-signed certificate.
- Operators must have an authenticated TLS option for public or managed PostgreSQL services.
- The administrative and read-only explorer connections must use the same policy.
- Invalid or unsupported values must fail with an actionable configuration error.
- Changing the policy must invalidate already-cached explorer connections.
- A policy change that persists before service reinitialization fails must still be reported accurately and audit logged.
- Connection errors and logs must identify the service, host, database, and selected mode without exposing passwords.

---

## Decision

### 1. Make `ssl_mode` a typed, closed set

Replace the free-form `Option<String>` at the query boundary with a `PostgresTlsMode` enum. Keep the serialized field name `ssl_mode` for API and stored-config compatibility.

| Serialized value | Transport | Certificate verification | Intended use |
|---|---|---|---|
| `disable` | Plaintext only | Not applicable | Explicitly trusted local/private networks where the server does not offer TLS |
| `require` | TLS required | Certificate and hostname are not verified | Self-signed Temps-managed or private PostgreSQL servers |
| `verify-full` | TLS required | Chain and hostname verified against trusted roots | Public or managed PostgreSQL services |

Each mode selects exactly one connection path. There is no retry using another mode.

This guarantee must also be enforced inside `tokio-postgres`, not only in
Temps' outer dispatch. `tokio_postgres::Config` defaults to `SslMode::Prefer`;
with that default, the driver may continue on the raw stream when PostgreSQL
answers the SSL negotiation request with `N`. The connector must construct the
configuration programmatically and set:

- `tokio_postgres::config::SslMode::Disable` for `disable`; and
- `tokio_postgres::config::SslMode::Require` for both `require` and
  `verify-full`.

No mode may inherit the driver default or accept a caller-supplied connection
string that overrides this setting. For either TLS mode, a server response that
declines TLS is a terminal connection error before a PostgreSQL startup message,
credentials, or queries are sent.

This closes a second, independent downgrade path. The current connector builds
a libpq-style DSN string with `format!()`, embedding host, username, password,
and database directly. A credential value containing whitespace followed by a
`key=value` token — for example a password containing `sslmode=disable` — can
be parsed by `tokio-postgres` as a separate connection parameter, silently
overriding a mode that was set correctly elsewhere. The connector must
therefore stop building a DSN string entirely and construct
`tokio_postgres::Config` exclusively through its builder methods (`.host()`,
`.user()`, `.password()`, `.dbname()`, `.ssl_mode()`), so no credential or
configuration value is ever parsed as a connection-parameter token. This
requirement applies to every new connection path introduced by this ADR, with
no DSN-string fallback for any mode.

The value `require` deliberately follows libpq semantics: it guarantees encryption but not server identity. The API schema and UI must label this clearly as “TLS, certificate not verified,” not as fully secure TLS. `verify-full` is the recommended choice whenever the server has a certificate rooted in a public or platform-trusted CA.

The legacy values `allow` and `prefer` are not retained because both permit downgrade. Parsing is deliberately split into two entry points so compatibility cannot leak into new writes:

- the create/update request parser accepts only `disable`, `require`, and `verify-full` and rejects every other value;
- the stored-config compatibility parser maps existing `allow` and `prefer` values to `require` and emits a structured warning naming the service and deprecated value.

Unknown stored values always fail closed. The compatibility parser is private to configuration loading and cannot be used by request DTO deserialization or parameter validation.

### 2. Preserve explicit compatibility without preserving silent downgrade

Existing records with `ssl_mode` absent are interpreted as `disable`, matching the provider's current persisted default. This avoids silently breaking Temps-managed PostgreSQL instances that do not offer TLS, while making their plaintext posture explicit and visible.

New service creation must persist a value rather than relying on deserialization defaults. The choice is based only on server-owned provenance, never inferred from the hostname, IP range, DNS suffix, or whether a TLS probe happens to succeed:

- a Temps-managed PostgreSQL service that is provisioned without server-side TLS persists `disable`;
- a service provisioned with Temps self-signed TLS persists `require`;
- every user-supplied, imported, or otherwise non-managed datasource must include an explicit `ssl_mode`; the UI recommends `verify-full` and does not pre-authorize a weaker mode.

Internal provisioning code identifies a Temps-managed service through a server-owned creation path, not a client-writable parameter. The public create API cannot claim managed provenance. Imported-container metadata such as `container_name` also does not imply a safe network or a TLS posture.

The provenance boundary must be a type-level distinction, not a boolean or optional field threaded through one shared constructor. Managed provisioning and user-facing creation call separate internal functions taking separate input types — for example `ManagedPostgresCreate`, constructed only by internal provisioning code, and `UserPostgresCreate`, the only type the external API handler can construct. The external handler must have no code path capable of producing a `ManagedPostgresCreate` value, so managed-mode defaulting cannot be reached by a client request regardless of what fields that request supplies. This mirrors the project's existing permission-guard boundary pattern rather than relying on a runtime flag that a future edit could route through the wrong path.

The service details API and dashboard must display the effective mode. Selecting `disable` or `require` must show a warning describing the missing protection. The update remains an audited write through the existing external-service update path.

### 3. Pass one policy through both explorer connection phases

`QueryService` resolves the effective `PostgresTlsMode` once from the decrypted service parameters. It passes the same value to:

- the administrative connection inside `ensure_readonly_user`; and
- the final `temps_explorer` connection.

Provisioning failure may still cause the existing fallback from the read-only role to the configured admin user, but that user fallback must not alter the TLS mode. Credential choice and transport policy are independent decisions.

`PostgresSource::connect` gains a required TLS-mode argument and dispatches to separate helpers for plaintext, encryption-only TLS, and verified TLS. The current TLS-then-plaintext match is removed. A mode-specific failure returns immediately.

The two TLS helpers also set the underlying `tokio_postgres::Config` to
`SslMode::Require`. Removing the visible TLS-then-plaintext match while leaving
the driver at its `Prefer` default would preserve the same downgrade one layer
lower. The plaintext helper alone uses `SslMode::Disable` and `NoTls`.

### 4. Separate encryption-only and authenticated TLS implementations

The current accept-all verifier is not retained unchanged. It currently accepts both the server certificate and TLS 1.2/1.3 `CertificateVerify` signatures unconditionally. That is broader than the intended `require` semantics.

The replacement encryption-only verifier skips certificate-chain and hostname trust in `verify_server_cert`, but delegates TLS 1.2 and TLS 1.3 handshake-signature verification to rustls using the presented certificate key and the configured crypto provider's supported algorithms. A server that cannot prove possession of the certificate's private key must be rejected. This verifier exists only behind `require` and must never be used by `verify-full`.

`verify-full` uses `rustls-platform-verifier`, already available in the workspace, as the single trust source. It validates the certificate chain against platform trust and verifies the requested DNS name or IP address. Failure to initialize or load platform trust is a connection failure; it must not fall back to bundled roots or the encryption-only verifier. IP-address connections must verify an IP subject alternative name and must not disable identity verification as a convenience.

Custom private CA bundles are deferred. A future addition may add an optional CA bundle to the same encrypted service configuration and extend `verify-full`; it must not introduce a process-wide environment variable or weaken verification when parsing the bundle fails.

`connect_with_self_signed_tls`, the function underlying the current accept-all verifier, is called directly today by the admin role-reconciliation path (`postgres_role_reconciler.rs`) and by cluster health probes. Hardening those call sites to use the typed mode system is deferred (see Deferred work), but leaving the function `pub` is not: as part of this ADR, `connect_with_self_signed_tls` is reduced to `pub(crate)` within `temps-query-postgres`, with a doc comment marking it as the legacy unauthenticated-trust path pending the health-probe follow-up. This is a mechanical, non-behavioral change scoped to this PR — it prevents new external callers from reaching the old accept-all behavior through a still-public function while the typed `PostgresTlsMode` API is the only path exposed to `temps-providers` and beyond.

### 5. Make policy changes generation-gated and terminate stale transports

`QueryService` currently caches connections by `(service_id, database)` and returns a cached connection before reading current service configuration. Without invalidation, an operator could change `ssl_mode` while the old transport remains active indefinitely.

Simple handler-side cache removal is insufficient for three reasons:

- a concurrent connection attempt can begin before removal and insert its old-policy connection afterward;
- `ExternalServiceManager::update_service` persists encrypted configuration before reinitializing the service, so it can return an error after the new mode is already committed;
- `PostgresSource::close` is currently a no-op and its connection task is detached, so removing the cache entry does not terminate clones or their transport.

`QueryService` therefore maintains a monotonically increasing policy generation per service. Each cached connection records the generation under which it was created. A create attempt captures the generation before loading configuration and rechecks it immediately before cache insertion; if the value changed, it terminates the new transport and retries from current configuration. An entry from an older generation is never returned or reinserted.

When an update request contains `ssl_mode`, the handler advances the service generation before invoking `ExternalServiceManager::update_service`. On both the success path and every error path, including failure after encrypted configuration persistence, it invalidates and terminates connections from older generations before releasing the update gate. Advancing before the update may cause a harmless reconnect when validation fails, but it prevents an old-policy connection from surviving an ambiguous partial update.

The service layer must expose persistence state as a typed outcome rather than forcing the handler to infer it from an error string. The update contract distinguishes at least:

- rejected before persistence;
- persisted and initialized successfully; and
- persisted but reinitialization failed, including the service ID, previous mode, applied mode, and typed cause.

For the third outcome, the handler invalidates stale transports, writes an audit event stating that the TLS policy was persisted and that reinitialization failed, and returns a Problem response that explicitly says the configuration changed despite the operational failure. It must not claim or imply rollback. A failure before persistence may be audited as a failed attempt under the existing audit policy, but it must not be recorded as an applied policy change.

Generation changes and connection creation use a per-service update gate. This avoids a global lock: unrelated services continue normally, while creation for the affected service either completes under the old generation before invalidation or observes the new generation and reloads configuration.

`PostgresSource` also gains an explicit transport shutdown handle owned alongside the client. Invalidation removes the cache entry, marks the source unusable, cancels or aborts the spawned `tokio_postgres` connection task, and waits for termination with a bounded timeout. Operations holding an old `Arc<PostgresSource>` fail after invalidation and cannot continue sending queries. The implementation must define a bounded drain policy for an operation already in flight; after the drain deadline it forcibly terminates the transport and returns a contextual interruption error.

The update endpoint must not report that the new policy is active until invalidation has completed. An invalidation failure is logged with the service ID and policy generation and returned as a contextual server error, while the source remains marked unusable so it cannot re-enter the cache. If configuration was already persisted, its audit event records the invalidation failure as part of the applied change.

This service-scoped generation design avoids a configuration read and decrypt on every explorer operation.

### 6. Surface the effective policy and actionable failures

Connection diagnostics must include `service_id`, host, port, database, and TLS mode as structured fields. They must never include the password or a full connection string.

Representative failures include:

- `disable`: plaintext connection failed for PostgreSQL service 42 at `db.internal:5432/app`;
- `require`: TLS is required for service 42, but the server rejected TLS or the handshake failed;
- `verify-full`: certificate verification failed for service 42 and hostname `db.example.com`.

The latter two errors must suggest changing configuration only when appropriate. They must not automatically retry with `require` or `disable`.

Audit data for a persisted mode change includes the service ID, previous and applied modes, policy generation, whether reinitialization succeeded, and whether stale-transport invalidation completed. It excludes credentials, certificate contents, and connection strings. Audit creation is part of completing the write path; an audit-storage failure returns a contextual server error and emits a structured security log rather than silently dropping the record.

This is a deliberate deviation from the codebase's general handler pattern, where an audit-write failure is logged but does not fail the primary operation. TLS-policy changes are the exception: an unrecorded transport-security downgrade or upgrade is itself a security-relevant gap, so for this write path specifically, an operator must be able to trust that "the update succeeded" implies "the change is audited." Implementers must not default to the general log-and-continue pattern here.

---

## Validation and Tests

Implementation is incomplete until the following tests pass.

### `temps-query-postgres` unit and integration tests

- Parse all three supported values and reject unknown values.
- Map legacy `allow` and `prefer` values to `require` only when reading existing configuration.
- Prove `disable` makes only a plaintext connection attempt.
- Prove `require` succeeds with a self-signed certificate and never retries plaintext after TLS failure.
- Against a protocol fixture that answers the PostgreSQL SSL request with `N`,
  prove both `require` and `verify-full` fail immediately and send no startup
  message, credentials, or queries on the raw stream. This specifically guards
  against accidentally restoring `tokio-postgres`'s default `SslMode::Prefer`.
- Prove `require` rejects an invalid TLS 1.2 or TLS 1.3 handshake signature even though it does not validate certificate trust.
- Prove `verify-full` accepts a trusted certificate with a matching hostname.
- Prove `verify-full` rejects an untrusted certificate, a DNS-name mismatch, and an IP SAN mismatch without retrying another mode.
- Prove failure to initialize platform trust fails closed without switching trust sources or modes.
- Prove transport shutdown terminates the connection task and makes outstanding source clones unusable.
- Verify errors contain endpoint and mode context and never contain the password.
- Verify no connection path constructs a libpq DSN string; a credential value containing an embedded `key=value` token (e.g. a password containing `sslmode=disable`) must not change the effective TLS mode.
- Verify `connect_with_self_signed_tls` is not reachable outside `temps-query-postgres` (compile-time visibility, not a runtime check).

TLS integration tests may use a local test server or a Docker-backed PostgreSQL fixture. Docker-dependent tests must skip gracefully at runtime when Docker is unavailable; they must not use `#[ignore]`.

### `temps-providers` service tests

- Verify `QueryService` forwards the configured mode to both the admin and explorer connections.
- Verify provisioning fallback from explorer user to admin user preserves the TLS mode.
- Verify missing configuration resolves to the documented compatibility mode.
- Verify strict create/update parsing rejects downgrade-capable and unknown values while the private stored-config parser alone accepts legacy aliases.
- Verify non-managed creation requires an explicit mode and client input cannot claim managed provenance.
- Verify changing `ssl_mode` terminates every cached connection for the service without affecting other services.
- Verify a connection created concurrently with an update cannot insert or reuse an old generation.
- Verify an update that fails after configuration persistence returns the typed partial-commit outcome, advances the generation, terminates old-policy connections, and records the applied policy change plus reinitialization failure in the audit log.
- Verify the HTTP error for a persisted-but-failed update says that configuration changed and never claims rollback.
- Verify an in-flight operation drains within the bound or is interrupted, and its stale source cannot re-enter the cache.
- Verify the generated parameter schema exposes the enum, descriptions, default behavior, and warnings.
- Verify the external API handler has no code path that can construct a `ManagedPostgresCreate` value; only internal provisioning code can.

Security-sensitive implementation requires `security-auditor` sign-off. Rust changes must pass `cargo test --lib -p temps-query-postgres`, `cargo test --lib -p temps-providers`, and `cargo check --lib` without warnings.

---

## Consequences

### Positive

- A handshake failure can no longer silently expose PostgreSQL credentials or data over plaintext.
- Self-signed deployments remain supported through an explicit encryption-only mode.
- Public database services can use authenticated TLS with hostname verification.
- The policy is per service, encrypted at rest, updateable at runtime, visible to the operator, and audit logged.
- Existing service records remain operable because an absent mode retains the documented plaintext behavior.

### Negative

- Existing `allow` or `prefer` configurations may stop connecting if their server does not support TLS; this is an intentional fail-closed change.
- `require` remains vulnerable to active server impersonation because it accepts any certificate. The UI and API documentation must not hide that limitation.
- Updating `ssl_mode` generation-gates connection creation and can interrupt active explorer operations after a bounded drain period.
- Verified TLS may require operators to fix certificate names or trust chains that were previously bypassed.

### Deferred work

- Provisioning server-side TLS for every Temps-managed standalone PostgreSQL service.
- Per-service custom CA bundle support for private PKI.
- Applying the same typed trust model to PostgreSQL health probes and cluster-control connections that currently call `connect_with_self_signed_tls` directly. Those paths do not perform plaintext fallback, but their certificate trust remains intentionally unauthenticated and should be audited separately. Restricting the function's visibility to `pub(crate)` is in scope for this ADR (see Decision, section 4); only rewriting those callers onto the typed mode system is deferred.

---

## Alternatives Considered

### Remove the plaintext fallback and always require TLS

Rejected because existing Temps-managed PostgreSQL services may intentionally run without TLS on a private host network. A hard switch would turn a security fix into an unplanned availability break and remove operator control.

### Keep TLS-first fallback and improve the warning

Rejected because logging does not prevent downgrade. The transport decision must be made before connecting and must not change in response to attacker-controlled handshake behavior.

### Add a global environment variable

Rejected because trust is a property of each datasource. A global setting cannot safely represent a mix of local plaintext, self-signed private, and publicly trusted databases, and runtime environment configuration is forbidden for this class of setting.

### Add a new database column

Rejected for now because PostgreSQL provider parameters already contain `ssl_mode` and the complete document is encrypted. A new column would duplicate the source of truth without improving security or operability.

### Treat every successful TLS handshake as verified TLS

Rejected because encryption without identity verification does not prevent an active attacker from presenting their own certificate. The product must distinguish `require` from `verify-full` honestly.

---

## Implementation Sequence

1. Introduce and test the typed mode, strict request parser, and private legacy stored-config parser.
2. Split the PostgreSQL connector into the three non-fallback paths, preserve handshake-signature verification, and add downgrade regression tests.
3. Add real transport shutdown and bounded in-flight draining to `PostgresSource`.
4. Thread the mode through both `QueryService` connection phases.
5. Add per-service policy generations, update gating, and race-safe cache insertion/invalidation.
6. Validate provider create/update provenance and inputs, then expose the field in the parameter schema.
7. Add API/dashboard status and warnings.
8. Complete security review and run the crate-level tests and workspace library check.

---

<!-- Maintenance: Review when PostgreSQL connection setup, provider parameters, rustls trust roots, or explorer caching changes. Owner: temps security/providers. Last reviewed: 2026-08-10. -->
