# Implementation Plan: OIDC SSO (Community Tier)

**Companion to:** [ADR-015](../015-oidc-sso-authentication.md)
**Date:** 2026-05-15
**Author:** David Viejo

## Goal

Minimal-effort OIDC login. Browser never sees IdP tokens — they stay server-side. Reuse the existing session cookie + `sessions` table so the rest of the app needs zero changes. Login page gains one extra button when OIDC is configured.

## Non-goals

- Storing or refreshing IdP access tokens for API use (we discard them after the callback).
- Multi-IdP, SAML, SCIM, group→role mapping, enforcement toggle. (See ADR-015 "Not solved by this ADR".)
- JWKS rotation handling beyond a TTL cache.
- Linking OIDC to *existing* logged-in users from a settings page. First-login link-by-email only.

## Token handling principle

The IdP returns `id_token`, `access_token`, `refresh_token`. We:
1. Validate `id_token` to extract `sub` + `email`.
2. Mint a normal Temps session token (the same one password login produces).
3. **Drop the IdP tokens.** Not stored, not logged, not returned to the browser.

The browser cookie remains the existing opaque `session` cookie. No JWT in the browser. No bearer tokens in localStorage. The IdP is used once per login and forgotten.

If we ever need the IdP's access token (we don't, in Community), it would live in a new `oidc_tokens` table keyed by user_id, encrypted via `EncryptionService` — but that is out of scope.

## Dependency: one crate

Add `openidconnect = "3"` to `temps-auth/Cargo.toml`. It handles discovery, PKCE, ID token validation, JWKS, and nonce checks. Picking the maintained pure-Rust OIDC client costs us one dependency and saves us writing JWS validation by hand.

No other new crates. `reqwest` is already in the workspace.

## Database (one migration)

`temps-migrations/src/m_YYYYMMDD_HHMMSS_oidc_sso.rs`:

```sql
CREATE TABLE oidc_providers (
  id                       SERIAL PRIMARY KEY,
  name                     TEXT NOT NULL,
  issuer_url               TEXT NOT NULL,
  client_id                TEXT NOT NULL,
  client_secret_encrypted  BYTEA NOT NULL,
  scopes                   TEXT NOT NULL DEFAULT 'openid email profile',
  jit_provisioning         BOOLEAN NOT NULL DEFAULT TRUE,
  enabled                  BOOLEAN NOT NULL DEFAULT TRUE,
  created_at               TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at               TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

ALTER TABLE users
  ADD COLUMN oidc_subject     TEXT,
  ADD COLUMN oidc_provider_id INTEGER REFERENCES oidc_providers(id);

CREATE UNIQUE INDEX users_oidc_unique
  ON users(oidc_provider_id, oidc_subject)
  WHERE oidc_subject IS NOT NULL;

CREATE TABLE oidc_login_states (
  id             SERIAL PRIMARY KEY,
  state          TEXT NOT NULL UNIQUE,
  nonce          TEXT NOT NULL,
  pkce_verifier  TEXT NOT NULL,
  provider_id    INTEGER NOT NULL REFERENCES oidc_providers(id) ON DELETE CASCADE,
  return_to      TEXT,
  expires_at     TIMESTAMPTZ NOT NULL,
  created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX oidc_login_states_expires_at ON oidc_login_states(expires_at);
```

Two new entity files in `temps-entities/src/`: `oidc_providers.rs`, `oidc_login_states.rs`. Add `oidc_subject` + `oidc_provider_id` to the existing `users.rs` model.

`oidc_login_states` cleanup piggybacks on the existing session-cleanup loop in `auth_service.rs` — one extra `delete_many().filter(expires_at.lt(now))` call. No new background job.

## Backend changes

### New files

```
crates/temps-auth/src/
  oidc_service.rs        ~250 LOC — discovery cache, login start, callback handle, user resolve
  oidc_handler.rs        ~150 LOC — 4 routes + Problem mapping
  oidc_types.rs           ~60 LOC — request/response DTOs + utoipa schemas
  oidc_errors.rs          ~80 LOC — OidcError enum + From<OidcError> for Problem
```

### Modified files

- `crates/temps-auth/src/plugin.rs` — register `OidcService`, mount `oidc_handler::router()`.
- `crates/temps-auth/src/lib.rs` — re-export new modules.
- `crates/temps-auth/src/handlers.rs` — `email_status` handler returns a new field `oidc_providers: Vec<{id, name}>` so the login page knows whether to render the SSO button. No other handler changes.
- `crates/temps-entities/src/users.rs` — add the two columns.
- `crates/temps-migrations/src/lib.rs` — register the new migration.

That's it. Zero changes to `auth_service.rs`, `middleware.rs`, `permissions.rs`, sessions, MFA, audit, or any handler outside auth.

### Routes

```
GET  /auth/oidc/providers           # public — list enabled providers (id, name) for login page
GET  /auth/oidc/login/{provider_id} # public — 302 to IdP authorize URL
GET  /auth/oidc/callback            # public — IdP redirect target; sets session cookie, 302 to /
POST /admin/oidc/providers          # admin — create the provider (Community: 409 if one exists)
GET  /admin/oidc/providers          # admin — list (always 0 or 1 in Community)
PATCH /admin/oidc/providers/{id}    # admin — toggle enabled / jit / update secret
DELETE /admin/oidc/providers/{id}   # admin — delete
POST /admin/oidc/providers/{id}/test # admin — runs discovery + JWKS fetch, returns ok/err
```

The Community single-provider cap is enforced inside `POST /admin/oidc/providers`: if a row exists, return `409 Conflict` with `OidcError::ProviderAlreadyExists`. One line. Premium will remove this guard without a migration.

### Callback flow (the load-bearing 80 lines)

```rust
// GET /auth/oidc/callback?code=...&state=...
async fn callback(state_param, code) -> Result<Redirect, Problem> {
    // 1. Look up & consume the login state row (single-use; delete on read)
    let login_state = service.consume_login_state(&state_param).await?;

    // 2. Build OIDC client from cached provider config + discovery doc
    let client = service.client_for(login_state.provider_id).await?;

    // 3. Exchange code for tokens (PKCE verifier from row)
    let token_response = client.exchange_code(code)
        .set_pkce_verifier(login_state.pkce_verifier)
        .request_async(&http_client).await?;

    // 4. Validate ID token (sig via JWKS, iss, aud, exp, nonce)
    let id_token = token_response.id_token().ok_or(EmailClaimMissing)?;
    let claims = id_token.claims(&client.id_token_verifier(), &login_state.nonce)?;

    // 5. Extract sub + email; resolve to user (link by sub, then email, then JIT)
    let user = service.resolve_user(login_state.provider_id, claims).await?;

    // 6. ** Drop token_response here. Nothing about the IdP leaves this function. **

    // 7. Create a normal Temps session (reuses existing auth_service)
    let session_token = auth_service.create_session(user.id).await?;
    let headers = auth_service.create_session_cookie(&session_token, is_https);

    // 8. Audit log: LoginViaOidc { user_id, provider_id, ip, ua }
    audit_service.create_audit_log(...).await.ok();

    // 9. 302 to return_to or /
    Ok((headers, Redirect::to(login_state.return_to.unwrap_or("/"))))
}
```

Step 6 is the explicit token-discard. We don't even bind `access_token` to a variable — `openidconnect`'s API gives us `.id_token()` directly, and the rest of `token_response` is dropped when the function returns. No persistence path, no log line that carries it.

### `resolve_user` logic

```rust
async fn resolve_user(provider_id, claims) -> Result<User, OidcError> {
    let sub = claims.subject().as_str();
    let email = claims.email().ok_or(EmailClaimMissing)?.as_str();

    // 1. By (provider_id, sub) — the stable path for returning users
    if let Some(u) = users::find_by_oidc(provider_id, sub).await? { return Ok(u); }

    // 2. By email — first-time OIDC for an existing local user. Link and return.
    if let Some(u) = users::find_by_email(email).await? {
        users::set_oidc(u.id, provider_id, sub).await?;
        return Ok(u);
    }

    // 3. JIT create, if enabled on the provider
    let provider = providers::get(provider_id).await?;
    if provider.jit_provisioning {
        return users::create_oidc_user(email, provider_id, sub).await;
    }

    Err(OidcError::UserNotProvisioned { email: email.into() })
}
```

MFA: `create_session` is the same function password login calls. If the user has MFA on, `create_session` already redirects to the MFA challenge flow. OIDC inherits this for free.

### Discovery cache

```rust
struct DiscoveryCache {
    inner: Mutex<HashMap<i32 /* provider_id */, (CachedClient, Instant)>>,
}
```

TTL: 1 hour. Lookup: if present and fresh, return; else fetch discovery + JWKS, build `CoreClient`, insert, return. Failures during a login attempt: return `OidcError::DiscoveryFailed` → 503 with "OIDC provider unreachable, try again or use password login." 30-second timeout on the discovery HTTP call.

The "Test connection" admin endpoint forces a re-fetch (bypasses cache) and returns the success/error so an operator can validate config without a real login round-trip.

## Frontend changes

### Updated: `email-status` response shape

Existing `GET /auth/email-status` already drives the login page (knowing whether the email exists, magic-link enabled, etc.). Add one field:

```ts
type EmailStatus = {
  // ...existing fields...
  oidc_providers: { id: number; name: string }[]  // empty array if none configured
}
```

The login page already fetches this; no new request needed.

### Updated: `login-form.tsx`

Three additions, all minimal:

1. Above the email/password form, render one button per `oidc_providers` entry (in practice: 0 or 1):

```tsx
{oidcProviders.map(p => (
  <Button
    key={p.id}
    variant="outline"
    className="w-full"
    onClick={() => { window.location.href = `/auth/oidc/login/${p.id}` }}
  >
    Sign in with {p.name}
  </Button>
))}
{oidcProviders.length > 0 && (
  <div className="my-4 flex items-center gap-2 text-xs text-muted-foreground">
    <div className="h-px flex-1 bg-border" /> or <div className="h-px flex-1 bg-border" />
  </div>
)}
```

2. `window.location.href` (not `fetch`) is deliberate — the backend issues a 302 to the IdP, which is a top-level navigation. No XHR.

3. Show a banner if the URL contains `?error=oidc_failed&reason=...` (the callback handler redirects to `/login?error=oidc_failed&reason=...` on failure, so the user sees what happened instead of a blank page).

### New: admin settings panel

`web/src/pages/settings/Auth.tsx` (new file, ~200 LOC):

- Card "OIDC provider."
- Empty state: "Connect an OIDC provider" button → dialog with fields (Name, Issuer URL, Client ID, Client Secret, JIT toggle). Submit → `POST /admin/oidc/providers`.
- Filled state: shows config (secret masked as `***`), Enabled toggle, JIT toggle, "Test connection" button (calls `/test`, shows result), "Delete" button.
- Pre-baked help copy with example issuer URLs for Authentik / Pocket-ID / Keycloak / Google Workspace. Static text, four lines, saves a Discord question.

OpenAPI codegen (`bun run openapi-ts`) regenerates the client after backend lands. No hand-written API helpers.

### CLI

One new command, `temps auth oidc set`, in `apps/temps-cli/`. Wraps `POST /admin/oidc/providers`. Reads client secret from stdin or a `--client-secret-stdin` flag so it doesn't show up in shell history. ~50 LOC, follows existing CLI command patterns.

## Effort estimate

| Area | LOC | Effort |
|---|---|---|
| Migration + entities | ~80 | 0.5 day |
| `oidc_service.rs` + errors | ~330 | 1.5 days |
| `oidc_handler.rs` + types | ~210 | 0.5 day |
| Plugin wiring + `email-status` field | ~30 | 0.25 day |
| Login form button + error banner | ~40 | 0.25 day |
| Admin settings panel | ~200 | 1 day |
| CLI command | ~50 | 0.25 day |
| Tests (unit + one integration) | ~400 | 1 day |
| Docs (operator setup for 4 IdPs) | — | 0.5 day |
| **Total** | **~1,340** | **~5.75 days** |

## Test plan

Unit (`oidc_service.rs`, mocked HTTP):
- `resolve_user` — happy path each of the three branches (by sub, by email, JIT).
- `resolve_user` — JIT off + unknown email → `UserNotProvisioned`.
- `resolve_user` — missing email claim → `EmailClaimMissing`.
- `consume_login_state` — expired row → `StateExpired`.
- `consume_login_state` — single-use (second call → `StateNotFound`).
- Discovery cache TTL respected.

Unit (`oidc_handler.rs`):
- `POST /admin/oidc/providers` — second insert → 409.
- Each `OidcError` variant → correct Problem status.

Integration (one, gated on Docker):
- Spin up Keycloak in a container (existing pattern from `temps-providers` tests).
- Create realm + client + test user.
- Drive the full flow: configure provider → start login → mock-browser-follow redirect → callback → assert session cookie set → assert user row created with `oidc_subject` populated.

Manual smoke test before merge: real Google Workspace + real Authentik, on a dev install. Confirm IdP tokens do not appear in any log line at TRACE level.

## Rollout

Behind a feature flag for one release (`TEMPS_OIDC_ENABLED=true`). Default off. After one release of real-world use, flag is removed.

Migration is forward-only and additive; rollback path is `DROP TABLE oidc_providers; DROP TABLE oidc_login_states; ALTER TABLE users DROP COLUMN oidc_subject, DROP COLUMN oidc_provider_id;` — written but not committed unless we need it.

## Security checklist (for `security-auditor` sign-off)

- Client secret stored only encrypted at rest via `EncryptionService`. Never returned by API; masked to `***`.
- `state` is single-use, 10-minute TTL, deleted on consume.
- PKCE verifier never logged.
- ID token signature verified via JWKS; `iss`, `aud`, `exp`, `nonce` all checked (`openidconnect` enforces by default — we add `aud == client_id` assertion).
- IdP `access_token` / `refresh_token` never persisted, never logged, dropped at end of callback handler.
- Open redirect: `return_to` is validated against same-origin before being used in the final 302.
- Email trust: we set `email_verified = true` on JIT-created users because the IdP attests to it. Documented as the trust assumption.
- Rate limit `/auth/oidc/callback` and `/auth/oidc/login/{id}` via the existing `rate_limit.rs` middleware.

## What we explicitly are not building

To resist creep:

- No "link OIDC to my existing logged-in account" UI flow. (First-login email match handles this for 90% of cases; the rest is documented as "log in with the matching email once.")
- No refresh-token storage. We do not call the IdP again after login.
- No group claim parsing. Users get the default role; admin promotes manually.
- No SSO-only toggle in Community. (See ADR-015.)
- No multi-IdP UI. The admin panel hides "Add another" once one exists.
