# ADR 049: Automatic expiry checks for structured credentials

**Status:** Accepted (Phases 1 and 2 implemented)
**Date:** 2026-10-03
**Related:** credential checks for environment variables and secrets, and X.509
certificate inspection, which were introduced alongside this ADR

## Context

Temps creates credential checks automatically when an environment variable or
a secret is saved. The pipeline is already shared between the two sources:

- `temps-credential-checks::automatic_check(candidates, value)` is the single
  policy entry point. It returns `AutomaticCheck::Certificate` when the value
  holds a parseable X.509 certificate, otherwise `AutomaticCheck::Http` when
  the value pattern identifies exactly one reviewed public issuer.
- `HttpChecksService::apply_automatic_check` (`http_checks/automatic.rs`) is
  called from both `variable_history.rs` and `secret_history.rs`; the only
  difference is the `Subject` (`env_var_id` + `env_check_suppressions` vs
  `secret_id` + `secret_check_suppressions`).
- `verify_row` resolves the value from whichever source the row points at and
  hands it to the same verifier.
- `http_checks.kind` is `http` or `certificate` (CHECK constraint added in
  `m20261002_000001_secret_checks_and_history`). At most one automatic check
  exists per credential (unique partial index on the subject column where
  `automatic_provider IS NOT NULL`).

Token-shaped credentials (GitHub, OpenAI, Stripe, ...) are the same whether
they are stored as an env var or a secret, and the HTTP presets already cover
both. What secrets add is **shape**: they are mounted as files under
`/run/secrets/<KEY>`, so they routinely hold structured, multi-line
credentials -- certificate chains, kubeconfigs, signing keys -- that carry
their own expiry. The same content also appears in env vars, usually
base64-encoded (`KUBECONFIG_B64`, `TLS_CERT_B64`).

A stored value never changes between checks, so for anything inspected
locally, **only time-dependent properties are worth monitoring**. Expiry is
the property that matters. Static defects (a key that does not match its
certificate) are validation at save time, not monitoring.

The feature branch was not merged when this was decided, so its migration and
the `certificate` kind were changed in place.

## Decision

### 1. One shared implementation, no source-specific code

Every new inspector lives in `temps-credential-checks` and is reached only
through `automatic_check()`. `apply_automatic_check` and `verify_row` gain no
per-format or per-source branches. A credential produces the same automatic
check whether it is an env var or a secret; a parity test enforces this
(section 9).

### 2. Generalize the `certificate` kind into a `local` kind

Replace `CheckKind::Certificate` with `CheckKind::Local` ("inspected on this
host; nothing is transmitted"), and `CertificateCheckSpec` with
`LocalCheckSpec { warning_days: Vec<u16> }` (same shape and defaults, so a
stored certificate spec parses unchanged). `CERTIFICATE_PROVIDER = "x509_certificate"` becomes
`LOCAL_PROVIDER = "local_expiry"`.

One local kind, not one kind per format, because:

- a single value can hold several formats (a kubeconfig holds certificates
  *and* a JWT), and only one automatic check may exist per credential;
- adding a format must not need a migration, a CHECK-constraint change, a new
  OpenAPI enum value and a new UI branch;
- suppression semantics stay simple: deleting the automatic local check of a
  credential suppresses local inspection for that credential.

`AutomaticCheck::Certificate` becomes `AutomaticCheck::Local(LocalCheckSpec)`,
check name "Credential expiry".

### 3. Artifact extraction layer

New module `temps-credential-checks/src/local/` (`mod.rs` plus one reader per
format: `x509`, `jwt`, `kubeconfig`, `openpgp`, `ssh`, and `armor` for
`-----BEGIN` blocks):

```rust
pub struct ExpiringArtifact {
    pub kind: ArtifactKind,   // X509Certificate | SshCertificate | OpenpgpKey | Jwt
    /// Human label, never secret material or token claims:
    /// "Certificate 'svc.example.test'", "Kubeconfig user 'ci' token",
    /// "OpenPGP key 0x0123456789ABCDEF", "SSH user certificate 'deploy'".
    pub label: String,
    pub not_before: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
}
pub struct Inspection {
    pub artifacts: Vec<ExpiringArtifact>, // earliest expiry first
    pub findings: Vec<Finding>,           // malformed blocks, revocations, short-lived tokens
}

/// Bounded (64 KiB, 16 items), local, no I/O. Items without an expiry are omitted.
pub fn inspect(value: &str) -> Inspection;
pub fn has_expiring_artifact(value: &str) -> bool; // replaces contains_certificate
```

A value is normalized into candidate views: as stored (indented blocks and CRLF
included), with literal `\n` escapes expanded, and base64-decoded (as both text
and binary, since DER and binary OpenPGP can happen to be valid UTF-8). Every
reader runs on each view; the first view that yields expiring items wins.
Containers (kubeconfig) hand their parts to the leaf readers.

PEM blocks are split by label before anything is decoded, so an unrelated block
(an encrypted legacy private key with RFC 1421 headers, say) never makes the
rest of the value unreadable. A malformed certificate block next to a good one
is reported as a finding instead of hiding the good one.

### 4. Inspectors

| Format | Recognition | Expiry source | Notes |
|---|---|---|---|
| X.509 (existing) | PEM `CERTIFICATE` blocks | `notAfter` | Unchanged behaviour |
| JWT | Whole trimmed value is three base64url segments whose header decodes to JSON with `alg` | `exp`, plus `nbf`, `iat` | Signature is **not** verified; only `exp`/`nbf`/`iat` are read and no other claim is ever returned, logged or stored (they can be personal data). No `exp` → no artifact. |
| Kubeconfig (container) | YAML or JSON containing `kind: Config` (the exact word, so a `ConfigMap` is not one); `clusters`, `users` and their entries may be null, as kubectl writes them | Parts: `clusters[].cluster.certificate-authority-data`, `users[].user.client-certificate-data` (X.509), `users[].user.token` (JWT) | Never follow file-path fields (`client-certificate`, `certificate-authority`, `tokenFile`) and ignore `exec` / `auth-provider` entries -- the value must never cause a host read or a process spawn. Labels use the user/cluster entry name. |
| OpenPGP key | ASCII-armored `PGP PUBLIC KEY BLOCK` / `PGP PRIVATE KEY BLOCK` (CRC-24 verified when present), or binary packets stored base64-encoded | Key expiration time from the newest self-signature of the primary key (issuer checked against the key ID, so third-party certifications are ignored) and the newest binding signature of each subkey | Expiry is readable without the passphrase. Only version 4 and 6 keys are read; a version 2, 3 or 5 key gets an `Unknown` `openpgp_unsupported_version` finding rather than a guessed expiry. Binary packets have no armor or checksum, so they are only read as a key when the primary key carries a self-signature naming its own key ID, and old-format packets of indeterminate length are rejected; random base64 that happens to frame as packets is therefore never mistaken for a key. A subkey is not reported once longer-lived keys together provide every capability it has (a replaced, expired subkey is history); if any of its capabilities would be left without a key, it is reported. Revoked key → `Error` finding; revoked subkeys are skipped. Labels use the key ID, never the user ID (personal data). |
| SSH certificate | `*-cert-v01@openssh.com` public-key line | `valid_before` (`u64::MAX` = forever → no artifact), `valid_after` as not-before | Plain SSH keys have no expiry and produce nothing. Verified against real `ssh-keygen` output for Ed25519, RSA and ECDSA certificates. |

OpenPGP and SSH certificates are read by small readers in this crate rather
than rPGP and `ssh-key`. Only packet framing, creation times, signature
subpackets and certificate validity fields are read; key material is never
interpreted and no signature is verified, so the cryptographic stacks those
crates bring would be dead weight on a small host. The only new dependencies
are `sha1` and `sha2` (both already in the lockfile) to derive OpenPGP key IDs,
and `serde_yaml` (already a workspace dependency) for kubeconfigs.

Every inspector reuses `expiry_finding` so finding codes stay identical to the
HTTP checks (`expired`, `expires_within_{n}_days`, `expiration_healthy`) and
alarms/UI need no new codes.

One additional JWT finding: when `exp - iat` is under 24 hours, emit
`short_lived_token` (`Warning`): "This token is valid for less than a day; a
session token stored as a long-lived credential will stop working." It sits
alongside the expiry finding, it does not replace it.

### 5. Verification and aggregation

`LocalVerifier::verify(value, now)` runs `inspect` and reports, in order: an
`artifacts_found` summary ("Found 2 certificates and 1 JWT."), the inspection
findings, then for each item earliest-first a `not_yet_valid` error when it
applies and one expiry finding. The overall status is the worst finding. A value
that no longer contains any expiring item yields `Error` /
`credential_not_inspectable`; one over 64 KiB yields `credential_too_large`, and
more than 16 items `too_many_items`. Format-specific problems are
`certificate_invalid`, `openpgp_invalid`, `openpgp_key_revoked`,
`openpgp_unsupported_version`, `ssh_certificate_invalid` and
`kubeconfig_invalid`. The verifier takes no
transport parameter, so it cannot perform network I/O.

### 6. Automatic policy

`automatic_check` becomes:

```rust
if let Some(preset) = automatic_preset(candidates, value) {
    return Some(AutomaticCheck::Http(Box::new(preset)));
}
has_expiring_artifact(value).then(|| AutomaticCheck::Local(LocalCheckSpec::default()))
```

An issuer preset only matches when the whole value is one recognized token, so
structured values (certificates, keys, kubeconfigs) never reach an issuer and
fall through to local inspection, as certificates did before. The order only
matters for a token that is both an issuer token and a JWT: it keeps the issuer
check, which verifies access as well as expiry. Manual checks still win over
automatic ones, and suppressions are still honoured -- `apply_automatic_check`
does not change.

### 7. Migration

`m20261002_000001_secret_checks_and_history` was edited in place
(unreleased): the CHECK constraint is `kind IN ('http','local')`. The same
migration brings env vars to parity with secrets, which section 1 requires:

- env vars scanned before local checks existed are re-scanned once (their
  detection markers are cleared, except where every automatic check is
  suppressed with `'*'` and a re-scan could not create one);
- re-pointing a manual check at another credential clears the detection
  markers of both the credential it now reads (so its automatic check is
  replaced, as on insert) and the one it stopped reading (so that credential
  can get its automatic check back).

A new env var value already resets its checks' results through the
`http_checks_credential_rotated` trigger (`m20260921_000001`); secrets get the
same reset in this migration, so rotation needed no env var change.

### 8. API and UI

- `CheckKind` OpenAPI value `certificate` → `local`.
- The detection response (`SecretDetectionView`, and its env var
  counterpart) replaces `certificate_detected: bool` with
  `local_artifacts: Vec<ExpiringArtifact>`. It never contains values or
  claims.
- `SaveHttpCheck.certificate` becomes `SaveHttpCheck.local`.
- Regenerate both clients (`cd apps/temps-cli && bun run spec:update && bun
  run generate:api`; `cd web && bun run openapi-ts`).
- The check card lists each artifact with its expiry (for example "Kubeconfig user
  'ci' client certificate -- expires 2027-01-04"); one shared component for
  env vars and secrets.
- Discoverability: when a value has no recognized artifact and no issuer, the
  detection panel lists which formats are inspected automatically instead of
  showing nothing.

### 9. Testing

All fixtures are generated in-test (rcgen certificates, hand-built JWTs,
OpenPGP packets and SSH certificates built to the RFC 9580 / PROTOCOL.certkeys
layouts) with generic names -- no captured real credentials.

Unit tests (`temps-credential-checks`):

- Each inspector: expiry thresholds map to the same codes as
  `expiry_thresholds_match_http_check_codes`.
- JWT: no `exp` → no artifact; `exp - iat < 24h` → `short_lived_token`;
  malformed segments → nothing; no claim other than exp/nbf/iat appears in
  any finding text.
- Kubeconfig: certificate + token → earliest wins, both listed;
  `exec`-only user → no artifact; path fields are never read; base64-wrapped
  kubeconfig is recognized.
- OpenPGP: subkey expiring before primary → subkey reported; a subkey whose capabilities are only partly kept by longer-lived keys → still reported; revoked key →
  `Error`; encrypted private key still yields expiry; binary packets whose
  primary key has no self-signature are ignored; version 3 and 5 keys are
  reported as unsupported; thousands of random base64 values yield no key.
- Kubeconfig: null `clusters`/`users` entries are accepted; a `ConfigMap` is
  not read as a kubeconfig.
- SSH certificate: `valid_before = u64::MAX` → no artifact.
- Bounds: input over 64 KiB, more than 16 artifacts, YAML alias expansion
  ("billion laughs") rejected, deeply nested YAML rejected, random bytes never
  panic.

Integration tests (`crates/temps-monitoring/tests/http_checks.rs`):

- **Parity**: the same value saved as an env var and as a secret produces an
  automatic check with the same kind, provider, spec and result.
- Rotating an issuer token into a certificate switches the automatic check
  from HTTP to local; a manual check replaces the automatic one; deleting the
  automatic check creates a suppression that survives the next rotation.
- A new value resets the check result, and re-pointing a manual check
  re-runs detection for both credentials.

### 10. Phasing

1. **Phase 1** (done): rename the kind to `local`, add the artifact layer, move
   X.509 behind it, JWT inspector, kubeconfig container, API/UI updates.
2. **Phase 2** (done): OpenPGP and SSH certificate inspectors.
3. **Phase 3** (not started) (save-time validation, not monitoring; surfaced as warnings in
   the detection response): certificate/private-key mismatch within one PEM
   value, AWS `ASIA…` temporary access keys and other known short-lived
   formats stored as long-lived values.

## Consequences

### Positive

- Kubeconfigs, signing keys and session-style JWTs, which commonly cause
  outages, get expiry alerts with no operator input and without sending
  anything off the host.
- New formats are added in one crate with no migration or API enum change.
- Env vars and secrets stay on one code path.

### Negative

- A JWT's `exp` is read without verifying the signature. That is acceptable
  for a reminder but means the check must never be presented as proof of
  validity.
- The OpenPGP and SSH readers are new code that handles untrusted input. They
  are bounded (64 KiB input, 4096 packets, 16 items), memory-safe, never
  allocate from a length field without the bytes being present, and are
  exercised by a deterministic random-input test.

### Risks

- Noise from JWTs that are intentionally short-lived and refreshed by the
  app. Mitigation: `short_lived_token` is a `Warning`, and the existing
  delete-to-suppress path applies.
- Re-scanning every env var once after upgrade runs at the reconciler's normal
  pace (20 per 5-second tick) and only creates checks a fresh save would have
  created.

## Alternatives Considered

- **One check kind per format** (`certificate`, `jwt`, `kubeconfig`, ...):
  rejected. It conflicts with one automatic check per credential, and each
  format needs a migration and an API enum change.
- **Secret-only inspectors**: rejected. Env vars hold the same content
  base64-encoded, and the checkers must be shared.
- **Following kubeconfig file paths / running `exec` plugins** to reach more
  credentials: rejected. A stored value must never cause a host read or a
  process spawn.

## Out of scope (follow-ups)

- Expiry from issuer responses for HTTP presets (for example GitHub's
  `github-authentication-token-expiration` header via the existing
  `ResponseField::Header`). It is shared with env vars and belongs to the
  HTTP preset catalog.
- PKCS#12 / keystores. Their certificates are normally password-encrypted, so
  a check needs a reference to a second secret holding the password.
- Files that wrap tokens (`docker config.json`, `.npmrc`, `.netrc`).
  Verifying the inner token would relax the rule in `detection.rs` that an
  embedded token never authorizes transmission. This needs its own decision.
- Multi-variable recipes (AWS key pair via STS, Azure client secret, GCP
  service-account JSON).
- Opt-in rotation-age alerts based on `secret_history` / `env_var_history`.
- Credentials Temps holds itself (Git connections, DNS providers, S3,
  SMTP).
