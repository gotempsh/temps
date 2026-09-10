# ADR-015: OIDC SSO as Community-Tier Authentication Method

**Status:** Proposed
**Date:** 2026-05-15
**Author:** David Viejo

> **Amendment (2026-07-17): Magic-link login removed.** Passwordless magic-link
> login has been removed from `temps-auth` entirely. It had no first-party
> consumer — the login screen never offered it and no client read its
> availability flag — and it was a live unauthenticated login endpoint, i.e.
> pure attack surface. Wherever this ADR names magic link as the account-recovery
> path for an SSO-only user or an IdP-down outage, that role is now served by the
> **password-reset flow**, which can set an initial password on a passwordless
> SSO account. The coexistence and "cannot be disabled in Community" arguments
> below still apply, but to **email/password** as the sole non-OIDC method.

## Context

Temps' current authentication surface (`crates/temps-auth/src/auth_service.rs`) is password + magic-link + MFA + session cookies, plus per-user GitHub OAuth used only for repo access (not login). Every developer who runs Temps gets the same login screen: email + password.

That is fine for a solo developer but a non-starter for the segments we actually want to convert at the Community tier:

- **Small dev teams** running self-hosted Temps already have Google Workspace or GitHub for identity. They do not want to maintain a second password database for "the deploy tool."
- **Self-hosted indie hackers** are increasingly using SSO providers (Authentik, Authelia, Pocket-ID, Zitadel, Keycloak) as their personal identity hub. "Does it speak OIDC?" is a routine pre-install question in our Discord.
- **Evaluators on the path to Premium/Enterprise** expect at minimum to point Temps at their existing IdP during a POC. If we make them stand up local users to evaluate, we lose them before they get to the paid features.

The full per-tier feature ladder we have committed to is:

- **Community (free, open source):** OIDC/SSO as an *auth method* — connect Temps to any OIDC IdP so users sign in with their existing identity. No multi-IdP, no enforcement, no SCIM, no SAML.
- **Premium:** Teams, RBAC, scoped tokens, audit log API. Still uses Community's OIDC as the auth method.
- **Enterprise:** SSO *enforcement* (cannot disable, cannot log in any other way), SAML in addition to OIDC, SCIM auto-provisioning, custom roles, tamper-evident audit. The enforcement, multi-protocol, and provisioning layer is where the paid value lives.
- **Custom:** Air-gapped IdPs, FIPS, BYO identity quirks.

This ADR is scoped tightly to the **Community** row: ship the OIDC client that lets a self-hosted operator point Temps at a single IdP and have their users log in with it. Everything that distinguishes the paid tiers (enforcement, SAML, SCIM, multi-IdP, JIT role mapping at scale, tamper-evident audit of identity events) is explicitly *not* in scope here.

The strategic reason for putting raw OIDC in the open-source build, rather than gating it behind Premium, is that "does the free tier do SSO at all" is the wrong question to lose to Coolify/Dokploy/Vercel on. The right question for us to win on is "does the free tier do *enforced, audited, multi-protocol, provisioned* SSO" — and that answer is no, and that is the upgrade path. Giving away basic OIDC removes a binary objection at the top of funnel without cannibalizing the features that actually justify $399+/mo.

## Decision

Add a single-tenant OIDC client to `temps-auth` that lets a self-hosted operator configure one OIDC provider and have users sign in through it. Email is the identity key. Existing email/password and magic-link flows stay; OIDC is an additional method, not a replacement, in the Community tier.

### 1. Scope of the Community feature

What ships in the open-source build:

- **One OIDC provider per Temps install.** Configured via env vars or the admin UI. The operator points Temps at *one* IdP (their Google Workspace, their Authentik, their Keycloak realm, their GitHub Enterprise OIDC, etc.).
- **Auth Code flow with PKCE.** No implicit flow, no resource-owner password grant. Standard OIDC discovery (`/.well-known/openid-configuration`).
- **`openid email profile` scopes only.** No group claims consumed, no role mapping, no custom claim handling. Whatever email comes back is the identity.
- **JIT user creation on first successful login,** if the operator has enabled it. Otherwise login fails with a clear "user not provisioned" error and an admin has to create the user first. Default is JIT-on for ergonomics; operators who care can flip it off.
- **Email is the join key.** If an OIDC subject arrives with an email that matches an existing local user, the accounts are linked. We do *not* support multiple OIDC identities per user in Community.
- **Coexistence, always.** Email/password, magic link, and OIDC all work in parallel. The login screen shows whatever methods are configured. **The operator cannot disable password or magic-link login in Community** — those paths stay on. Turning them off is the Enterprise capability, because the value of "enforced SSO" is not the flag itself but the auditable, tamper-evident guarantee that the flag cannot be quietly flipped back. Community has neither the audit substrate nor the policy guarantees to make that promise, so we do not ship the toggle.

What is explicitly out of scope and stays paid:

- Multiple simultaneous IdPs.
- SAML.
- SCIM provisioning / deprovisioning.
- Group → role mapping. (Community OIDC users get the default role; admins manually adjust afterwards.)
- **Enforcement.** Disabling password/magic-link login is not available in Community at all — not as a toggle, not as an env var, not as a CLI flag. Operators who want SSO-only login upgrade to Enterprise.
- Tamper-evident audit logs of identity events.
- Just-in-time *role* provisioning from IdP claims.

### 2. Data model

One new table, `oidc_providers`, designed for exactly one row in Community but with a schema that the paid tiers can extend to N rows without a migration:

| Column | Type | Notes |
|---|---|---|
| `id` | `i32` PK | |
| `name` | `text` | Display name shown on the login button ("Sign in with Authentik") |
| `issuer_url` | `text` | OIDC discovery base, e.g. `https://auth.example.com` |
| `client_id` | `text` | |
| `client_secret_encrypted` | `bytea` | AES-256-GCM via `EncryptionService` |
| `scopes` | `text` | Default `openid email profile` |
| `jit_provisioning` | `bool` | Whether unknown emails get a user created |
| `enabled` | `bool` | Soft on/off without deleting config |
| `created_at` / `updated_at` | `DBDateTime` | |

`users` gets two nullable columns added in the same migration:

- `oidc_subject` (`text`, nullable, unique with `provider_id`)
- `oidc_provider_id` (`i32` FK to `oidc_providers`, nullable)

A user with a non-null `oidc_subject` is "linkable" to that provider. Existing users keep working unchanged. We do **not** rename or repurpose the existing `oauth_states` table (which is for GitHub repo OAuth) — OIDC login state is a separate concern with separate lifecycle, and reusing it would create confusing coupling.

A new `oidc_login_states` table holds the per-attempt nonce/PKCE verifier with a 10-minute TTL, cleaned up by the existing session-cleanup loop in `auth_service.rs`. Same shape as `oauth_states` but distinct, because the rows mean different things ("an in-flight login" vs. "a repo-link consent") and conflating them tomorrow when we add the second provider type would be messy.

The Community-tier UI enforces "at most one row in `oidc_providers`" at the handler level (not at the DB). Paid tiers will remove that check.

### 3. Login flow

A standard OIDC Authorization Code + PKCE flow living in `temps-auth`:

1. User clicks "Sign in with `{provider.name}`" on the login page.
2. Handler `GET /auth/oidc/login` generates `state`, `nonce`, PKCE `code_verifier`/`code_challenge`, writes a row to `oidc_login_states` with 10-minute expiry, and 302s to the IdP's authorization endpoint (looked up via cached OIDC discovery).
3. IdP redirects back to `GET /auth/oidc/callback?code=…&state=…`.
4. Handler validates `state` (exists, not expired, single-use — row is deleted on consume), exchanges `code` for tokens at the token endpoint, validates the ID token (signature against JWKS, `iss`, `aud`, `exp`, `nonce`).
5. Extract `sub` and `email`. Find user by `(provider_id, sub)` or fall back to email match. If neither matches and `jit_provisioning = true`, create the user with `email_verified = true` (we trust the IdP's verification). If JIT is off, return a typed `AuthError::UserNotProvisioned { email }` rendered as a 403 with a clear message.
6. Issue a normal Temps session — same `sessions` table, same cookie, same expiry, same MFA enforcement. **OIDC login does not bypass MFA** if MFA is enabled on the user account; the IdP's MFA does not satisfy ours in the Community tier (Enterprise can revisit this with stronger claim trust).
7. Audit-log the login as `LoginViaOidc { user_id, provider_id, ip, user_agent }`. The audit trail stays in the existing `audit_logs` table — we are not building tamper-evident logging here.

Discovery, JWKS, and token endpoints are fetched on demand and cached in-memory with a 1-hour TTL and a 30-second fetch timeout. Discovery failures during a login attempt return a 503 with "OIDC provider unreachable" — fail closed.

### 4. Errors and observability

A new `OidcError` enum in `temps-auth`, following the project's typed-error pattern. Specifically:

```rust
pub enum OidcError {
    NoProviderConfigured,
    DiscoveryFailed { issuer: String, reason: String },
    StateNotFound { state: String },
    StateExpired { state: String, age_secs: i64 },
    TokenExchangeFailed { status: u16, body: String },
    IdTokenInvalid { reason: String },
    UserNotProvisioned { email: String },
    EmailClaimMissing,
    Database(#[from] sea_orm::DbErr),
}
```

Each maps to a specific Problem Details response (400/401/403/503), with structured fields for grep-ability. Every callback failure logs at WARN with the IdP issuer, the failure mode, and the (already-burned) state — enough to debug a real customer's broken integration without leaking secrets.

### 5. Admin UX

`/settings/auth` gets a panel:

- "OIDC provider" card. Empty state has a button: "Connect an OIDC provider." Filled state shows issuer, name, scopes, enabled toggle, JIT toggle, and "Test connection" (does a live discovery + JWKS fetch and reports back).
- "Login methods enabled" summary: password, magic link, OIDC — each with an on/off.
- Pre-baked help links in the empty state for the four IdPs we know our users actually run: Authentik, Pocket-ID, Keycloak, Google Workspace. (The provider doesn't care which one it is — the help text saves people from filing Discord questions.)

The CLI gets one new command: `temps auth oidc set --issuer ... --client-id ... --client-secret-stdin` so operators can configure SSO before the web UI exists in their install (chicken-and-egg on fresh deploys).

### 6. Coexistence with existing auth

- Password login stays the default. Operators opt *in* to OIDC.
- Existing sessions are unaffected.
- A user with both a password and an OIDC link can use either. We do not force account merging.
- MFA enforcement is per-user, not per-method. OIDC login still asks for the TOTP / recovery code if the user has MFA on.
- Magic links keep working for everyone, including OIDC-only users (acts as an account-recovery path if the IdP is down).
- Password and magic-link login cannot be disabled in Community. The login screen always exposes whichever of password / magic-link the install was deployed with, plus OIDC if configured. The "SSO-only" experience is an Enterprise feature.

## Consequences

### Positive

- Removes the "does it do SSO?" objection at the top of funnel without giving away the paid SSO surface.
- A self-hosted operator with Authentik / Pocket-ID / Keycloak / Google Workspace can land Temps and sign in with their existing identity in under five minutes.
- Schema is forward-compatible: when Enterprise adds multiple IdPs, SAML, SCIM, and enforcement, no migration is needed on the columns we add now — only additions.
- Email-as-join-key matches what every other small-team SaaS does. No identity-mapping cliff for users moving from password to OIDC.
- The `oidc_providers` row + `oidc_subject` column on users is the same place where Premium will hang group-claim parsing, Enterprise will hang `enforce_sso`, and SCIM will hang `external_id`. We are building the foundation, not a throwaway.

### Negative

- Adds an external dependency at login time. If the IdP is down, OIDC users cannot log in via that path; they need the magic-link fallback. We surface this clearly in the UI but it is a real availability story we have to support.
- Email-as-join-key has a known sharp edge: if a user's email changes at the IdP, we will create a duplicate Temps user on next login. We document this and rely on the admin merging accounts manually. Premium/Enterprise will fix this properly via SCIM / stable subject lookup.
- The discovery + JWKS cache is one more piece of state that can be stale. A rotated signing key at the IdP could break logins for up to one hour. Mitigated by short TTL + manual "Test connection" button that re-fetches.
- The temptation to creep features into Community (group claims, multi-IdP, "just one more thing") will be constant. The decision rule going forward is: anything that touches *enforcement*, *audit guarantees*, *automated provisioning*, or *multi-protocol* belongs in the paid tiers, regardless of how easy it would be to build into the Community path.
- Operators who want SAML will be told "Enterprise." That is a real conversion friction we accept because SAML is, in fact, what Enterprise buyers buy.

### Not solved by this ADR

- **SSO enforcement.** Community cannot disable password / magic-link login at all. Enterprise adds the operator-facing toggle, the hash-chained audit of that policy, the customer-controlled anchor sink, and the guarantee that the toggle cannot be flipped back silently. Out of scope here.
- **SAML.** Different protocol, different lifecycle, different tooling (XML signing, metadata exchange, IdP-initiated flows). Belongs in the SAML ADR that ships with Enterprise.
- **SCIM provisioning and deprovisioning.** Push-based user management from the IdP. Belongs in the SCIM ADR that ships with Enterprise.
- **Group → role mapping.** Reading `groups` claim and assigning RBAC roles. Requires RBAC (Premium) and stable claim trust (Enterprise). Out of scope; Community users get the default role and admins promote manually.
- **Multiple IdPs.** Schema supports it, handler does not. Unlocked in Premium/Enterprise.
- **Subject migration when a user's IdP changes.** Belongs in the SCIM / identity-mapping ADR.
- **MFA delegation to the IdP.** Trusting the IdP's MFA claim instead of ours requires per-claim trust policy. Enterprise-only.
