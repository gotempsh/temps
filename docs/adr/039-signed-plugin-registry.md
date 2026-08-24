# ADR-039: Cryptographically-Signed Cloud Registry for External Plugin Discovery

## Status

Proposed

## Context

### Current design and why it needs to change

`crates/temps-external-plugins/src/handler.rs` maintains `KNOWN_PLUGINS`, a compile-time `&[KnownPlugin]` constant that is the single source of truth for which plugins a temps instance can install. Each entry carries a `manifest_url` — a fixed trusted string the installer fetches to learn version and download hashes. The constant exists precisely because the manifest URL is the trust root for the whole install flow: whoever controls that URL controls what binary gets installed and executed. Accepting an arbitrary URL from an HTTP caller would be SSRF and RCE by design, so the hardcoded constant is the correct security boundary for that threat.

The limitation is operational: adding a new installable plugin, or updating an existing entry's manifest URL, requires recompiling and redeploying the temps binary. For a project targeting "single binary, self-hosted" operators who may be pinned to an older release, this means new plugins in the catalog are invisible until the operator upgrades — bad for discoverability, bad for the marketplace trajectory.

The security invariant `KNOWN_PLUGINS` enforces is correct and must be preserved. What changes is *where* the curated list lives: instead of a compile-time constant, it becomes a remotely-fetched document whose authenticity is guaranteed by a digital signature from the developer who authored it. The temps binary still never acts on an arbitrary untrusted URL; it acts on content verified against a trust anchor it knows at compile time.

### Threat model for the registry fetch

- **Network-level attacker (MITM)**: Can substitute a different response body over the wire. Defense: signature verification rejects anything not signed by a trusted key.
- **Compromised registry host** (temps.sh CDN or Next.js deployment): Can serve an attacker-chosen document. Defense: the document must still carry a valid signature from the developer's private key, which never touches the server at runtime.
- **Compromised private key**: Catastrophic — can publish any binary. Defense: keep the key in a CI secret that never leaves GitHub Actions; key rotation is a compile-time trust anchor update + registry re-sign.
- **Replay of a stale, older-version document**: Acceptable for v1 — no timestamp validity window needed initially. The registry is curator-controlled, not user-visible until the operator refreshes.
- **Air-gapped / no-network installs**: The operator has no access to temps.sh. Defense: the compile-time `KNOWN_PLUGINS` constant (from PR #728) is retained as a static fallback, giving zero-network installs the same catalog they have today.

### Relevant project constraints

- No runtime configuration as environment variables. Registry URL is an admin-configurable column on a DB entity.
- No new heavyweight dependencies. The dependency tree already contains `ed25519-dalek` 2.2.0 and `sha2` 0.10.9.
- Graceful degradation: features that depend on optional configuration must surface an onboarding state, not disappear.
- Three-layer architecture must be respected (Handler → Service → Data Access).

## Decision

### 1. Signature scheme: Ed25519 with a simple `{payload, signature, key_id}` envelope

Use **Ed25519** as implemented by `ed25519-dalek` 2.2.0 — already present in the workspace's Cargo.lock. This adds zero new transitive dependencies. Ed25519 has 32-byte public keys and 64-byte signatures (smaller than any RSA or ECDSA-P256 equivalent), verification is constant-time by construction, and `ed25519-dalek` 2.x is the canonical Rust implementation with a stable API.

Do not use JWS or JOSE. They add an indirect dependency on a JOSE library, introduce header-parsing ambiguity (algorithm confusion attacks), and are substantially more code for no benefit at this threat level. A minimal bespoke envelope is simpler to audit and simpler to implement on the signing side.

**Envelope format** (the document fetched from the registry URL):

```json
{
  "payload": "<base64url-encoded UTF-8 JSON bytes of the registry body>",
  "signature": "<base64url-encoded 64-byte Ed25519 signature over the payload bytes>",
  "key_id": "<opaque string matching a trust anchor entry>"
}
```

The `payload` field is the canonical registry body (defined in Decision 3), base64url-encoded without padding. The `signature` is computed over the raw bytes of `payload` (i.e. the base64url string bytes, not the decoded JSON) to make the signed surface byte-for-byte unambiguous — publisher and verifier agree on the exact byte sequence without any JSON normalization step.

`key_id` is an opaque short string (e.g. `"temps-2026-01"`) used to select the correct public key from the trust anchor table. It has no cryptographic weight; the signature is the proof.

### 2. Trust anchor model: embedded compile-time table, evolvable without rearchitecture

The temps binary embeds a compile-time constant table of trusted public keys:

```rust
struct TrustAnchor {
    developer_id: &'static str,  // e.g. "temps"
    key_id:       &'static str,  // e.g. "temps-2026-01"
    public_key:   [u8; 32],      // Ed25519 raw public key bytes
}

const TRUST_ANCHORS: &[TrustAnchor] = &[ ... ];
```

For v1, this table has exactly one entry — the Temps developer key. Adding a second developer (v2 marketplace scenario) requires adding a row to this constant and releasing a new binary. That is the correct bar for v1: the operator explicitly updates to a release that trusts a new publisher, giving them a clear audit trail.

When verifying a registry envelope, the binary looks up `key_id` in `TRUST_ANCHORS`, takes the corresponding public key, and verifies the signature. If `key_id` is absent from the table, verification fails regardless of the signature. This ensures that a key that was trusted in an older release but removed in a later one cannot be replayed against the newer binary.

**Path to v2 (self-serve publishers)**: The table stays. A future "marketplace key" entry would be a single root key whose corresponding private key is used by a server-side API to cross-sign per-developer keys. The per-developer key is embedded in the registry envelope alongside the signature (e.g. a `developer_cert` field containing a signature over `{developer_id, developer_public_key}` by the marketplace root key). The verifier chain would be: look up `key_id` → if it's the marketplace root, first verify `developer_cert` using the root key, then verify the envelope signature using the certified developer public key. This does not require rearchitecting the envelope or the trust anchor constant; it adds one new verification step.

Key rotation for the Temps key: add a new row to `TRUST_ANCHORS` with a new `key_id`, re-sign the registry with the new key, and remove the old row in a subsequent release after operators have had time to upgrade.

### 3. Registry payload format: fully inlined, no secondary fetches

The signed registry payload carries all per-plugin per-platform download information inline. There is no secondary per-plugin manifest fetch. This is the correct design because:

- One signature covers the entire install chain. The SHA-256 checksums for every platform binary live inside the signed document, so a MITM cannot swap out the download without invalidating the signature.
- Eliminating secondary fetches removes an entire attack surface class (SSRF from a signed-but-attacker-influenced URL).
- The registry document is small (kilobytes, not megabytes) even with dozens of plugins — there is no reason to paginate it.

**Registry body schema** (the JSON encoded as `payload` in the envelope):

```json
{
  "schema_version": 1,
  "generated_at": "2026-08-19T12:00:00Z",
  "plugins": [
    {
      "developer_id": "temps",
      "name": "vibetemps",
      "display_name": "VibeTemps",
      "description": "AI-assisted app builder embedded in the Temps platform.",
      "version": "1.2.3",
      "binary_name": "temps-vibetemps-plugin",
      "platforms": {
        "linux-amd64":  { "url": "https://...", "sha256": "<hex>" },
        "linux-arm64":  { "url": "https://...", "sha256": "<hex>" },
        "darwin-amd64": { "url": "https://...", "sha256": "<hex>" },
        "darwin-arm64": { "url": "https://...", "sha256": "<hex>" }
      }
    }
  ]
}
```

`developer_id` must match a `TrustAnchor.developer_id` entry whose `key_id` matches the envelope's `key_id`. This prevents one developer from publishing entries under another developer's name.

`schema_version` allows the parser to reject documents with an unsupported version rather than silently misinterpreting future fields.

`generated_at` is informational only in v1; a future version could refuse to trust documents older than N days.

### 4. Fetch, verify, and cache flow in the temps binary

**Registry URL configuration**: stored as a column `registry_url` on a new `external_plugins_settings` row (one row per server, singleton). Default value baked into the migration: `https://temps.sh/plugins/registry.signed.json`. Operators override it via the existing settings API pattern — an API/UI knob in the System Admin section, same path as OIDC provider config. The column is a trusted URL admins configure, not a secret, so it does not need `EncryptionService`.

**Caching**: After a successful fetch-and-verify, the verified envelope bytes (the whole `{payload, signature, key_id}` JSON, not the parsed/decoded body) are persisted to `$TEMPS_DATA_DIR/plugins/registry-cache.json`. The `ExternalPluginsService` holds an in-memory cache of the last verified `ParsedRegistry` with a timestamp. The registry is re-fetched at most once per hour during normal operation, triggered lazily on the first `GET /x/plugins/registry` request that finds the cache stale, not on a background timer. This is control-plane behavior (admin page load, not hot path), so a lazy refresh is appropriate.

**The disk cache is untrusted storage, not a trust boundary.** It holds the same signed envelope bytes that come over the network, and nothing about writing it to disk adds integrity beyond what the signature already provides. Anyone with filesystem write access to `TEMPS_DATA_DIR` (e.g. via an unrelated vulnerability) can overwrite it with an attacker-signed-or-unsigned document. Consequently, **every read of the disk cache runs the identical full verification pipeline (steps 2–8 below) that a fresh network fetch does — there is no "trusted because it was verified once before" shortcut.** If the cached envelope fails verification on read, it is discarded (not used, not re-cached) and the flow falls through to `KNOWN_PLUGINS` exactly as if no cache existed.

**Verification steps** (in order, all in `RegistryFetcher`, a new service in `temps-external-plugins`; this exact sequence runs identically whether the envelope bytes came from the network or from the disk cache):
1. Obtain envelope bytes (network fetch with a 30-second timeout and a 1 MiB response-size cap — reject before buffering, not after, since the registry document is expected to be tens of KB at most even with a large catalog — or a disk-cache read).
2. Deserialize the envelope struct.
3. Look up `key_id` in `TRUST_ANCHORS`. Return `RegistryError::UntrustedKey` if absent. Each `TrustAnchor` entry also declares its `algorithm` (only `Ed25519` exists today); reject if the looked-up anchor's algorithm isn't the one the verifier is about to use, so a future non-Ed25519 anchor added for v2 can't be silently misinterpreted by verification code that assumes Ed25519.
4. Base64url-decode (RFC 4648 §5, **no padding** — both the TypeScript signer and the Rust verifier are pinned to this exact variant, validated by the shared test vector described below) `signature` bytes.
5. Verify signature over the raw `payload` string bytes (the base64url string itself, pre-decode) using `ed25519-dalek::VerifyingKey::verify`.
6. Base64url-decode `payload` bytes, parse as `RegistryBody`.
7. Assert `schema_version == 1`; reject higher versions this binary doesn't understand.
8. Validate each plugin entry's `developer_id` matches the signing key's `developer_id` from the trust anchor.
9. Only once verification succeeds: persist the raw envelope bytes to the disk cache, updating the cache timestamp, and record `last_verification_failure_reason = None`.
10. Return the parsed `RegistryBody`.

**Cross-ecosystem correctness test vector**: because the signer (TypeScript, `sign-registry.ts` in temps-landing) and the verifier (Rust, this crate) must agree byte-for-byte on the base64url encoding with no room for "library default" drift, a fixed test vector — a known registry body, a known Ed25519 test keypair (not the production key), and the exact expected envelope JSON — is committed to *both* repositories and asserted against in CI on both sides. This is the only way a padding/alphabet mismatch is caught before it silently degrades every self-hosted instance to the `KNOWN_PLUGINS` fallback with no error surfaced anywhere except a log line. Key material note: `ed25519-dalek` 2.x `SigningKey::from_bytes` and `@noble/ed25519` both take a **32-byte** raw seed, not 64 — implementers must confirm this against the actual crate/package APIs in use (see Risk 1) rather than trusting the 64-byte figure that appeared in an earlier draft of this ADR.

**Failure modes and UI/install behavior.** The service tracks the *reason* for the last fallback (`FetchFailed` vs. `SignatureInvalid` vs. `SchemaUnsupported`), not just that a fallback occurred, because a benign network outage and an active integrity attack must not be handled identically by the install path even though the read path degrades the same way in both cases:

- **Fetch fails (network error / non-2xx / response exceeded the size cap)**: Use the disk cache if present, verifies successfully, and is not older than 24 hours. If no valid cache, use the compile-time `KNOWN_PLUGINS` fallback, converted into the same `ParsedRegistry` shape. The `GET /x/plugins/registry` response includes a `registry_source` field: `"live"`, `"cached"`, or `"fallback"`, plus a `registry_warning` human-readable string when degraded. The frontend `PluginsPage` renders a visible banner ("Plugin catalog loaded from cache — last updated N hours ago") when `registry_source` is not `"live"`. It never hides the plugin list. **`POST /x/plugins/install` is still permitted** in this state — a network hiccup must not block installs from a recently-cached, still-valid catalog.
- **Signature verification fails** (on either a network fetch or a disk-cache read): Log `ERROR` with the `key_id` value sanitized (control characters stripped before it reaches a log field — the value is attacker-influenced network input) and computed vs. expected details. Do not use a document whose signature fails under any circumstance. Fall through to the next source in the same order (disk cache, then `KNOWN_PLUGINS`) exactly as a fetch failure does for the *read* path — but additionally set `last_verification_failure_reason = SignatureInvalid`. Surface a visible admin alert: "Plugin registry signature verification failed. The last trusted catalog is shown." **`POST /x/plugins/install` checks `last_verification_failure_reason` and refuses with an explicit error — "Plugin install refused: the remote registry failed signature verification. Resolve the registry integrity alert before installing." — until a subsequent fetch verifies successfully and clears the flag.** This is the one place fetch-failure and signature-failure handling diverge: a bad signature is treated as an active-attack signal that blocks new installs even though it doesn't invalidate a catalog already known-good, while a mere network outage does not.
- **schema_version mismatch (too high)**: Fall back to cache/fallback list. Surface the same admin alert with "This temps version does not support the current registry format. Upgrade temps to see new plugins." Treated the same as a fetch failure for install-blocking purposes (this is a compatibility gap, not an integrity failure).

### 5. Cloud-side signing in temps-landing: build-time static artifact

The signed registry is a **static JSON file generated at deploy time** — it is not produced by a live API endpoint. The private key never resides in a running process.

**Workflow**:
1. The `kfsoftware/temps-landing-new` repository (private) contains a `scripts/sign-registry.ts` script.
2. The registry body JSON (the plaintext plugin catalog) is committed to the repo as `data/plugins-registry.json`. Updating the catalog means editing this file in a PR.
3. The signing script reads the private key from a GitHub Actions secret (`PLUGIN_REGISTRY_SIGNING_KEY`, stored as hex-encoded raw Ed25519 private key bytes), produces the `{payload, signature, key_id}` envelope, and writes it to `public/plugins/registry.signed.json`.
4. The signing step runs in the `deploy` GitHub Actions workflow, after `npm run build`, before the artifact is uploaded to the CDN/Vercel deployment.
5. The committed `data/plugins-registry.json` is the authoritative plaintext; the signed artifact in `public/` is build output and is not committed to the repo.

This approach means:
- The private key is a GitHub Actions secret — it exists only in memory during the CI run and never touches disk or a running server.
- The CDN serves a static file with no server-side logic required.
- The signing step is auditable (it is a script in the repo, not a black box).
- An accidental CDN compromise exposes a signed document the attacker cannot modify to install different binaries, because they do not have the private key.

**Key material storage rule**: The raw Ed25519 private key seed (**32 bytes**, hex-encoded = 64 characters — confirm against `ed25519-dalek::SigningKey::from_bytes` and whichever Node.js Ed25519 library `sign-registry.ts` uses before implementing; do not assume a 64-byte "seed+pubkey" format from other ecosystems) is stored exclusively as a GitHub Actions repository secret named `PLUGIN_REGISTRY_SIGNING_KEY`. It is never committed, never logged, and never transmitted to the CDN. A corresponding `PLUGIN_REGISTRY_KEY_ID` secret stores the `key_id` string (e.g. `"temps-2026-01"`) that maps to the trust anchor entry in the Rust binary.

**Signing-script supply chain**: the compromise of `PLUGIN_REGISTRY_SIGNING_KEY` is a strictly higher-value target than compromising any single CDN artifact — a stolen signing key lets an attacker publish a validly-signed registry pointing at any binary, for every self-hosted temps instance that trusts the corresponding public key, indefinitely (until the key is rotated out of `TRUST_ANCHORS` in a new release). `sign-registry.ts`'s own dependencies (the Ed25519 library it imports) are therefore part of this trust boundary, not incidental tooling: pin their exact versions with lockfile integrity hashes, enable Dependabot alerts scoped to this script's dependencies, and run the signing step in its own CI job with no other third-party Actions sharing that job's environment (so nothing else in the workflow can read `PLUGIN_REGISTRY_SIGNING_KEY` out of the process environment).

**Update workflow for today's single developer**: To release a new plugin version or add a new plugin, the Temps team opens a PR against `data/plugins-registry.json` in `temps-landing-new`, merges it, and the deploy pipeline signs and publishes the new registry. There is no separate publish UI. SHA-256 checksums for new binaries are computed by a helper in the same script from the built binary artifacts (or fetched from their GitHub Release pages). The first update of vibetemps after this ADR lands requires manually populating `data/plugins-registry.json` with the existing vibetemps entry from `KNOWN_PLUGINS`.

### 6. Out of scope for v1

The following are explicitly deferred and must not be added during this implementation:

- **Self-serve publisher signup and authentication**: No publisher account system, no API for submitting a signing key.
- **Per-developer key management UI**: No dashboard for rotating or revoking developer keys.
- **Certificate revocation lists (CRLs)**: Trust anchor removal via binary update is sufficient for v1.
- **Plugin review or moderation queue**: Temps curates the list manually. There is no submission workflow.
- **Marketplace browsing UI**: The `PluginsPage` continues to show a flat list, not a searchable storefront.
- **Timestamp validity window enforcement**: The `generated_at` field is present but not validated against wall-clock time in v1.
- **Per-version pinning in the install request**: The `version` field in `InstallPluginRequest` remains unused; install always fetches the version declared in the current signed registry.

### 7. Migration: KNOWN_PLUGINS becomes the static fallback

PR #728's `KNOWN_PLUGINS` constant is **not superseded**; it becomes the compile-time fallback catalog used when the signed registry is unreachable and no disk cache exists. This preserves its value for air-gapped operators and zero-network installations, and avoids discarding the work already done in the PR.

The `KNOWN_PLUGINS` constant entries are stripped of `manifest_url` (they no longer need it — install now reads from the signed registry body) and retain only `name`, `binary_name`, and the version/platform data that was previously in the per-plugin manifest. During implementation, the static fallback data is populated from whatever the current live vibetemps manifest says, then maintained manually alongside the signed registry going forward.

The `fetch_manifest` method in `install.rs` is removed. `PluginInstaller::install` receives a `PluginEntry` (from the parsed registry body, signed or fallback) instead of a separately-fetched `PluginRegistryManifest`. The external network call during install becomes solely the binary download — the metadata is already present in the verified registry.

## Consequences

### Positive

- The plugin catalog can be updated without recompiling or redeploying the temps binary, unblocking the path to a multi-plugin marketplace.
- The full install chain — catalog, checksums, and binary download — is covered by a single cryptographic signature from a known key. The security property is strictly stronger than the current model (manifest fetch was unauthenticated HTTPS, trusting the CDN's TLS certificate only).
- Air-gapped operators retain full functionality via the compile-time fallback.
- No new transitive Rust dependencies: `ed25519-dalek` 2.2.0 is already in the lock file.
- The trust anchor model is extensible to multiple developers without rearchitecting the envelope format or the verification flow.

### Negative

- Installing a newly-trusted developer's plugins requires a temps binary upgrade (the trust anchor is compiled in). This is intentional but is a deployment friction for third-party plugins in a future marketplace.
- Adding an entry to the catalog now requires coordination between the Rust binary release cycle (trust anchor update for new developers) and the temps-landing deploy pipeline (registry content). For v1 with one developer this is not a problem; at v2 it requires the marketplace root-key cross-signing extension.
- The disk cache adds a small amount of state to `TEMPS_DATA_DIR`. Operators who do not expect state outside their database may be surprised, though plugin binaries themselves are already written there.

### Risks

**Risk 1 — ed25519-dalek API assumptions.** The ADR assumes `ed25519-dalek` 2.2.0 (already in the lock file) exposes `VerifyingKey::verify(message_bytes, &Signature)` and does not require a context or pre-hashing step, and that `SigningKey::from_bytes`/the Node.js signing library both take a **32-byte** raw seed (not 64). **Verify both against the crate's/package's actual APIs before implementation starts** — confirmed by `security-auditor` review as a likely source of a silent, total signing failure if assumed wrong. The 2.x `ed25519-dalek` API changed significantly from 1.x (it now uses the `signature::Verifier` trait).

**Risk 2 — base64url without padding.** The envelope uses base64url-no-padding (RFC 4648 §5) for both `payload` and `signature`. The signing script (TypeScript in temps-landing) and the verifying code (Rust in temps) must use the same alphabet and padding convention — a mismatch produces a signature that always fails verification, degrading every self-hosted instance to `KNOWN_PLUGINS` with no error surfaced beyond a log line. Per `security-auditor` review, "pin it in comments" is not a sufficient mitigation on its own: a shared cross-ecosystem test vector (fixed test keypair, fixed registry body, fixed expected envelope bytes) is committed to and asserted against in CI in *both* repositories, so a drift is caught at PR time rather than in production.

**Risk 3 — Registry URL column bootstrap order.** Reading the registry URL from a DB column requires a DB connection and migration to have run. During first-ever startup (before migrations run) and in the event of a DB outage, the registry fetch must fall back to the compile-time default URL without attempting a DB read that would fail. The `RegistryFetcher` must handle the case where the settings row does not yet exist and use the baked-in default URL rather than propagating a DB error.

**Risk 4 — disk cache is untrusted, not pre-verified.** The disk cache at `$TEMPS_DATA_DIR/plugins/registry-cache.json` holds signed-envelope bytes identical in trust level to a fresh network response; anyone who gains filesystem write access to `TEMPS_DATA_DIR` through an unrelated vulnerability can plant an attacker-controlled document there. Per `security-auditor` review (rated HIGH — the review's top concern), the implementation **must** re-run the full verification pipeline on every disk-cache read, not just at write time; caching "verified" bytes must never be conflated with caching pre-verified trust. See the "disk cache is untrusted storage" note in Decision 4.

**Risk 5 — signature failure vs. network failure must not be handled identically at the install boundary.** Per `security-auditor` review (rated HIGH), a persistent MITM that always returns an invalid signature is indistinguishable from a network outage if both degrade the *read* path to cache/fallback and nothing else changes — that lets an attacker suppress a security-motivated catalog update indefinitely while an admin believes they're just seeing connectivity issues. `POST /x/plugins/install` tracks the last verification-failure reason and refuses to proceed on `SignatureInvalid` until a subsequent fetch verifies cleanly (see the divergent handling in Decision 4's failure-mode list), while a mere `FetchFailed` from a recently-verified cache still permits installs.

**Risk 6 (lower severity, tracked not blocking) — registry fetch has no response-size cap; downloaded plugin binaries have no size cap either** (the latter pre-dates this ADR, in `install.rs`'s `download_asset`). Both read the full body into memory before any validation, so a MITM or compromised CDN endpoint can trigger memory exhaustion on the reference 4 GB deployment before the signature/checksum check ever gets a chance to reject the content. Cap the registry fetch at ~1 MiB and the binary download at a generous-but-bounded ceiling (e.g. 512 MiB), rejecting mid-stream once the cap is exceeded rather than after the full body is buffered.

**Risk 7 (lower severity, tracked not blocking) — replay of a stale-but-validly-signed registry is not detected in v1** (the `generated_at` field is present but unvalidated, as stated in Decision 6's "out of scope" list). A MITM that captured an older signed registry can replay it indefinitely to withhold a security-motivated plugin update. Accepted as a v1 risk per the original design; revisit before or at the first security-motivated registry republish, not on an open-ended timeline.

**Risk 8 (lower severity, tracked not blocking) — `key_id` from an unverified envelope reaches log output on failure paths.** It is attacker-influenced network input; sanitize control characters before it reaches a structured log field to avoid log injection into SIEM/alerting pipelines.

## Alternatives Considered

### Option A: Minisign / Signify (simple file-signing tools)

Pros: very simple single-file signing and verification; a `minisign` Rust crate exists. Cons: the Rust verifier crates are less mature and less actively maintained than `ed25519-dalek`; the wire format is purpose-built for file signing and not easily adapted to a JSON envelope.

Rejected because `ed25519-dalek` is already a dependency and its API is well understood in the project's dependency graph.

### Option B: JWS / JOSE envelope

Pros: standard format, parseable by many libraries. Cons: requires adding a JOSE library to the Rust dependency tree (none currently present), introduces algorithm-negotiation complexity and the well-documented algorithm-confusion attack class, and is substantially more code for identical cryptographic strength. Rejected in favor of the minimal bespoke envelope.

### Option C: TUF (The Update Framework)

Pros: industry standard for software update security; handles key rotation, revocation, and threshold signatures natively. Cons: substantial implementation complexity (root, targets, snapshot, timestamp roles; metadata chain); adds significant new dependencies; complete overkill for a catalog with one developer and a handful of plugins. Rejected as premature for v1. If the marketplace grows to dozens of third-party developers with independent key management needs, migrating to TUF at that point is a reasonable future ADR.

### Option D: Keep KNOWN_PLUGINS, iterate faster via more frequent binary releases

Pros: zero new code, zero new attack surface. Cons: does not solve the discoverability problem for operators pinned to an older release; does not enable a third-party marketplace path at all. Rejected because the goal is specifically a dynamic registry.

## Implementation Notes

- **Affected crates**: `temps-external-plugins` (primary — new `RegistryFetcher` service, modified `PluginInstaller`, modified handler), `temps-core` (possibly a new `registry_crypto` module if the Ed25519 verify logic is shared with a future self-update signing flow), `temps-entities` / `temps-migrations` (new `external_plugins_settings` entity + migration for the registry URL column).
- **New crate dependency** (in `temps-external-plugins`): `ed25519-dalek` with feature `"verifying"` only (no signing in the binary). Confirm the feature flag name in the 2.2.x release.
- **temps-landing dependency**: the `sign-registry.ts` script needs `@noble/ed25519` or equivalent in Node.js/Bun for signing. The `data/plugins-registry.json` plaintext file is committed; `public/plugins/registry.signed.json` is build output.
- **Migration needed**: yes — adds `external_plugins_settings` table.
- **Breaking changes**: no. The install and reload API shapes (`InstallPluginRequest`, `InstallPluginResponse`, `PluginRegistryEntry`) are unchanged from PR #728. The `manifest` field of `PluginRegistryEntry` changes type from `PluginRegistryManifest` (the old separately-fetched struct) to the inlined `PluginEntry` from the signed registry body — this is a schema change in the `GET /x/plugins/registry` response, but that endpoint is new in PR #728 and not yet in a released binary, so it does not break any existing client.
- **Security review**: completed by `security-auditor` (2026-08-19). 9 findings total (2 HIGH, 4 MEDIUM, 3 LOW); all folded into Risks 1–8 above. The 2 HIGH findings (Risk 4: disk cache must be re-verified on every read, not trusted from write time; Risk 5: signature-verification failure must block new installs, not just degrade identically to a network outage) and the 2 MEDIUM correctness findings most likely to cause a silent total feature failure (Risk 1's 32-vs-64-byte key length, Risk 2's cross-ecosystem test vector requirement) are treated as pre-implementation blockers — implementer-rust must not diverge from the Decision 4 language above on these points without a follow-up security-auditor pass. Risks 6–8 are tracked but non-blocking.
- **PR #728 relationship**: implement this ADR in a follow-on PR that targets the `plugin-host-latest` branch or its successor. Do not attempt to land both in the same PR.
