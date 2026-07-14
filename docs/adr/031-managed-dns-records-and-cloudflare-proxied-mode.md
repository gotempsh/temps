---
title: "ADR-031: Managed DNS Records and Cloudflare Proxied Mode"
status: Proposed
date: 2026-07-12
author: David Viejo
---

# ADR-031: Managed DNS Records and Cloudflare Proxied Mode

**Status:** Proposed
**Date:** 2026-07-12
**Author:** David Viejo
**Security review required:** Yes — this feature writes to users' public DNS zones and stores provider API tokens. A bug can take a production domain offline or hijack traffic. Requires security-auditor sign-off before implementation.
**Related:** Issue #139 (flattened public hostname templates for proxied wildcard TLS), PR #146 (flat public hostname strategy — hard dependency), PR #270 (DNS wired into TlsService), `temps-dns` crate (`DnsProvider` trait, Cloudflare/Route53/GCP/Azure/DigitalOcean/Namecheap providers)
**Demand signal:** Outside contributor (bherila) with a hard requirement: never expose the origin server IP; all public traffic must go through Cloudflare's proxy. He currently cannot use temps-managed domains without manual DNS work per hostname, and reports having written conflict-resolution logic in his fork. Discussed 2026-07-12 (WhatsApp).

---

## Context

### The problem

Temps already has a multi-provider DNS abstraction (`temps-dns`): the `DnsProvider` trait exposes full record CRUD (`create_record`, `update_record`, `delete_record`, `set_record`, `remove_record`), zone listing, and a `DnsProviderCapabilities` struct that already models `proxy`, `auto_ssl`, and `wildcard`. Today this is used almost exclusively for ACME DNS-01 challenge TXT records. Nothing in temps creates the A/AAAA/CNAME records that actually point a domain at the server — users do that manually.

For a user whose threat model forbids exposing the origin IP, every hostname must be a **Cloudflare-proxied** record. Two things break:

1. **Manual toil per hostname.** Each custom domain, environment subdomain, and preview URL needs a proxied record created by hand in the Cloudflare dashboard.
2. **Cloudflare's Universal SSL depth limit.** Cloudflare's free/pro certificates only cover one subdomain level. A proxied `*.foo.example.com` fails TLS unless the user buys Advanced Certificate Manager (~$200/mo, per the contributor). The workaround is flattening: `*-foo.example.com` instead of `*.foo.example.com` — exactly what PR #146 implements as the "flat public hostname strategy."

A secondary risk raised in the discussion: if temps auto-provisions a public hostname + Let's Encrypt certificate per deployment, 100 deployments in a day exhausts LE rate limits. Behind the Cloudflare proxy this is unnecessary anyway — Cloudflare terminates public TLS, and the origin can serve a self-signed or origin certificate.

### Why the scary part is scary

Temps would be writing to zones it does not own. Users have existing records — MX, SPF, apex A records, records managed by other tools. Overwriting an unmanaged record is the one mistake this feature cannot make: self-hosted users debug alone, and a clobbered production DNS record is an outage they may not trace back to temps for hours.

## Decision

Add **managed DNS record automation** as an opt-in, per-domain feature on top of the existing `DnsProvider` trait, with an ownership-marking scheme that makes "never touch a record temps didn't create" structurally enforced, plus first-class Cloudflare proxied mode that composes with PR #146's flat hostname strategy.

### 1. Ownership marking (the core safety invariant)

Every record temps creates carries a machine-readable ownership marker: a companion TXT record holding typed JSON, e.g. `{"managed_by":"temps","instance":"<install_id>","record_type":"A","project_id":N,"environment_id":N,"v":1}` (the external-dns registry pattern). This works uniformly across all providers.

The registry name is **type-scoped and injective** (both properties exist because their absence lets temps clobber a record it never created — found in security review):

- Type-scoped: `_temps-owned-a.<name>`, `_temps-owned-aaaa.<name>`, `_temps-owned-cname.<name>`, … — owning `app` A never grants ownership of a user's `app` AAAA. The marker JSON additionally carries `record_type` and both must match.
- Injective escaping of the record name: `_` → `__` before `*` → `_w`, so `*.staging` and a literal `wildcard.staging` (or `_w.staging`) can never share a registry name.

*Implementation note (v1):* Cloudflare's per-record `comment` field was originally preferred there for dashboard visibility, but the `cloudflare` crate's DNS params don't expose it, so v1 uses the TXT registry on Cloudflare too. Comment stamping can be added later as a purely additive enhancement (the TXT registry stays authoritative).

Rules, enforced in the service layer, not left to callers:

- **Create:** refuse if a record with the target name/type already exists without a covering temps marker — AND refuse if the registry name itself is occupied by a TXT that is not our marker (the marker write must never upsert over foreign content, including another install's orphan marker).
- **Update/Delete:** only permitted when the marker parses, matches this temps instance, and covers the record type. Unparsable, foreign, or type-mismatched marker → refuse.
- **Conflict resolution UI:** on conflict, offer *import* (adopt the record: stamp it with a marker after explicit user confirmation) or *skip*. Default is always **never overwrite**. No bulk "overwrite all."
- **Marker hygiene:** the `instance` field is validated on parse (`[A-Za-z0-9-]`, ≤ 64 chars) so attacker-written TXT content can't inject into temps logs/UI.
- **Concurrency:** all guarded operations on the same (zone, name) are serialized in-process through a keyed async lock. The remaining remote TOCTOU window (DNS APIs have no compare-and-swap) is an accepted residual risk.

### 2. Provider-agnostic surface, one provider per zone

Record automation is configured per domain/zone, reusing the existing DNS provider credential records (encrypted via `EncryptionService`, per the no-env-var rule). One DNS provider per zone; the UI enforces this. Providers advertise support via the existing `DnsProviderCapabilities` — a zone on a provider without `a_record`/`cname_record` support falls back to today's manual instructions (`ManualDnsProvider` behavior).

### 3. Cloudflare proxied mode + flat hostname coupling

- `proxied: bool` on the managed-record config, only offered when `capabilities().proxy` is true.
- **Guardrail:** when a domain plan would create a proxied record at ≥2 subdomain levels (`*.foo.example.com`, `a.b.example.com`), temps must detect it, explain the Universal SSL depth limit in the error/warning, and recommend the flat hostname strategy (PR #146). Failing with Cloudflare's opaque 526/525 at request time is not acceptable.
- PR #146 is therefore a **merge prerequisite** for the proxied path.

### 4. TLS strategy behind the proxy

When a domain's records are proxied, the per-hostname Let's Encrypt flow is skipped by default. The origin serves a temps-generated self-signed certificate (Cloudflare Full mode) — this both removes the LE rate-limit exposure and matches how Cloudflare-fronted origins normally run. Authenticated Origin Pulls and Cloudflare Origin CA certificates are explicitly deferred (see Non-goals) but the config shape must not preclude them.

### 5. Deployment / preview URL policy

Per-project setting controlling what deployment and preview URLs get:

- **(a)** no public DNS record (default — internal/testing use only, today's behavior),
- **(b)** flat-scheme records under the managed zone (requires #146),
- **(c)** *(deferred)* a separate TLD, potentially on a different DNS provider, for non-prod.

v1 ships (a) and (b). (c) is a config-model consideration only: the setting is per-environment-class, not a single boolean, so adding (c) later is non-breaking.

### 6. Defaults and observability

- Record automation is **off** until a provider is explicitly connected and enabled per domain.
- Domain UI shows per-record state: `created` / `conflict` / `unmanaged` / `error`, each with the provider's actual error text.
- All record writes are audit-logged; reconciliation is O(changes) (triggered by domain/environment mutations), not a periodic full-zone rescan.

## Alternatives considered

- **Cloudflare-only integration (contributor's fork approach).** Fastest to his need, but temps already has six DNS providers behind one trait; a Cloudflare-specific path would fork the domain model and contradict the core-primitives philosophy.
- **No ownership marker, name-based matching only.** Simpler, but "temps deletes whatever matches the name" is exactly the clobbering failure mode. Rejected.
- **TXT-registry for all providers including Cloudflare.** Uniform, but the comment field is more visible in the Cloudflare dashboard (an operator sees *why* the record exists) and avoids doubling record count. Cloudflare uses comments; TXT is the generic fallback.
- **Keep LE per-hostname certs behind the proxy.** Works in Cloudflare Full (strict) only with valid origin certs and re-introduces rate-limit exposure per deployment. Default off behind proxy; still available for non-proxied records.

## Non-goals (v1)

- Cloudflare Origin CA certificate issuance and Authenticated Origin Pulls.
- Different TLD / different provider for non-prod deployments (5c).
- Managing records temps did not create, beyond the explicit one-at-a-time import flow.
- MX/SPF/any records unrelated to routing traffic to temps.

## Consequences

- The ownership scheme is the one-way door: once user zones contain temps-marked records, the marker format is a compatibility surface. It carries a `"v":1` field for that reason.
- PR #146 becomes load-bearing for the proxied path and must merge first.
- Providers gain no new trait methods for v1 — the work is a new orchestration service in `temps-dns`/`temps-domains` plus entities for per-domain automation config and record state.
- Security review must cover: zone-scoped token guidance in docs, marker spoofing (a foreign record with a forged temps marker — mitigated by `instance` install-id matching), and audit coverage of every write.
