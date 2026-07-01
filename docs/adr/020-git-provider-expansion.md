# ADR-020: Git Provider Expansion — Gitea, Bitbucket, and Generic Providers

**Status:** Proposed
**Date:** 2026-06-30
**Author:** David Viejo

> **ADR number note:** ADR-020 is also tentatively claimed by the parked
> `harden/multi-node-deployments` branch (project memory: `project_multinode_hardening_adr020`).
> Those two ADRs are independent in scope. This document should be renumbered to
> ADR-021 at merge time if the multi-node hardening ADR lands first; the
> implementer must check and reconcile before opening the PR.

> **Security review (2026-06-30): SIGN-OFF WITH CONDITIONS.** The
> `security-auditor` reviewed this design and approved it subject to five
> MUST-FIX conditions, folded into the relevant decisions below and listed in
> Section 14 ("Security review outcome"). This is a design sign-off, not a code
> sign-off — the listed items must be re-verified at PR review.

## Context

Temps is a self-hosted PaaS targeting developers and teams who want to own their
deployment infrastructure. The git integration layer currently supports GitHub
(including GitHub Apps) and GitLab (Cloud and self-hosted), both fully
operational. Three additional provider variants exist in the type system but
return `Err(GitProviderError::NotImplemented)` in
`GitProviderFactory::create_provider` (`git_provider.rs:796–808`):

- `GitProviderType::Gitea` — self-hosted Gitea and its downstream Forgejo
- `GitProviderType::Bitbucket` — Bitbucket Cloud and Data Center
- `GitProviderType::Generic` — any git server not otherwise supported

The sidebar IA for the CI/CD section lists: GitHub · GitLab · Bitbucket ·
Gitea (→ Integration) · Other Git Providers. All five must be reachable in the
UI. This ADR promotes the three stubbed variants from `NotImplemented` to
working providers, with explicitly tiered capability guarantees. The Generic
tier is reframed as "Manual Git Providers" and matches the Coolify/Dokploy
pattern for connecting arbitrary git hosts. **The Manual tier is HTTPS-only —
public repositories and token auth** (see Decision 3 for why SSH is out of
scope).

### What the existing code already provides

- `git_providers` table has `base_url: Option<String>` and `api_url:
  Option<String>`. Both are nullable, and the initial migration
  (`m20250101_000001_initial_schema.rs`) already comments `gitea` and
  `bitbucket` as valid `provider_type` values.
- `AuthMethod` enum (`git_provider.rs:83–112`) already contains
  `PersonalAccessToken`, `OAuth`, `BasicAuth { username, password }`,
  `GitHubApp`, and `GitLabApp` — covering every credential mode this ADR needs.
- `git_ops.rs` already implements `clone_repo` (no auth, for public repos) and
  `clone_repo_with_credentials(url, dir, username, token, branch)` at line 160,
  injecting HTTP Basic credentials via libgit2's callback.
- The SSRF guard in `handlers/base.rs:45–54` (`reject_ssrf_url` calling
  `temps_core::url_validation::validate_external_url`) already applies to every
  `base_url` and `api_url` at create time. A stricter HTTPS-only
  `validate_git_url` also exists in `temps-core/src/url_validation.rs`.
- `ConnectionHealthService` probes every active connection via `validate_token`.
- `GitPrCommenter::upsert_inner` (`pr_commenter.rs:180`) dispatches on
  `provider.provider_type.as_str()`.

### Provider capability tiers

This table is the canonical reference for what each provider supports at v1.
It maps directly to the sidebar IA.

| Capability | GitHub | GitLab | Bitbucket Cloud | Gitea / Forgejo | Manual (Generic) |
|---|:---:|:---:|:---:|:---:|:---:|
| Clone + deploy | Yes | Yes | Yes | Yes | Yes |
| Repository picker (sync) | Yes | Yes | Yes | Yes | No |
| Auto webhook registration | Yes | Yes | Yes | Yes | No |
| PR / MR comments | Yes | Yes | Yes | Yes | No |
| OAuth flow | Yes | Yes | No (v1) | No (v1) | No |
| Framework detect (no clone) | Yes | Yes | Yes | Yes | No |
| HMAC webhook signature | Yes | Yes | No | Yes | N/A |
| Public repo (no credentials) | Yes | Yes | Yes | Yes | Yes |
| HTTPS token auth | Yes | Yes | Yes | Yes | Yes |

"Manual (Generic)" intentionally offers only clone and deploy over HTTPS. It is
the universal escape hatch for providers not natively supported.

## Decision

### 1. Three new provider structs

#### 1a. `GiteaProvider` — `crates/temps-git/src/services/gitea_provider.rs`

Gitea (and its downstream Forgejo) exposes a REST API at `{base_url}/api/v1`
structurally similar to GitLab's `/api/v4`. Gitea is fully self-hosted; every
instance lives at an operator-chosen base URL.

`GiteaProvider` takes `base_url: String` and `auth_method: AuthMethod`. It
stores two pre-built `reqwest::Client` instances: a 30-second timeout client
for API calls, and a 15-minute total timeout client for archive streaming. Both
use `redirect::Policy::none()` as SSRF defense-in-depth, matching the pattern
in `GitLabProvider::get_client()` and `get_archive_client()`. An archive
redirect validation function (`validate_archive_redirect_host`) must enforce
HTTPS-only and restrict the host to the registrable domain of `base_url`,
exactly as `gitlab_provider.rs` does.

**Authentication:** PAT only for v1. Gitea authenticates PATs via
`Authorization: token {pat}`.

**Key API mappings:**

| Operation | Gitea API endpoint |
|---|---|
| Get user | `GET /api/v1/user` |
| List repos (paged) | `GET /api/v1/repos/search?limit=50&page=N` |
| Get repo | `GET /api/v1/repos/{owner}/{repo}` |
| List branches | `GET /api/v1/repos/{owner}/{repo}/branches` |
| List tags | `GET /api/v1/repos/{owner}/{repo}/tags` |
| Get file content | `GET /api/v1/repos/{owner}/{repo}/contents/{path}?ref={ref}` |
| Get commit | `GET /api/v1/repos/{owner}/{repo}/git/commits/{sha}` |
| List commits | `GET /api/v1/repos/{owner}/{repo}/commits?sha={branch}&limit={n}` |
| Create webhook | `POST /api/v1/repos/{owner}/{repo}/hooks` |
| Delete webhook | `DELETE /api/v1/repos/{owner}/{repo}/hooks/{id}` |
| Archive download | `GET /api/v1/repos/{owner}/{repo}/archive/{ref}.tar.gz` |
| Create repo | `POST /api/v1/user/repos` / `POST /api/v1/orgs/{org}/repos` |
| Create PR | `POST /api/v1/repos/{owner}/{repo}/pulls` |
| Create file | `POST /api/v1/repos/{owner}/{repo}/contents/{path}` |

**Pagination:** Gitea returns `X-Total-Count` and a `Link` header with
`rel="next"`. Override `list_repositories_page` for native streaming
pagination, matching the `GitLabProvider` pattern.

**Webhook signature:** `HMAC-SHA256(key=hook_secret, message=raw_body_bytes)`,
hex digest in `X-Gitea-Signature`. Structurally identical to GitHub's
`X-Hub-Signature-256`. Implement `verify_webhook_signature` using `hmac` +
`sha2` (already in the workspace dependency tree). Handler file:
`handlers/gitea.rs`, route: `POST /webhook/git/gitea/events`.

**Clone username:** `x-access-token` for HTTPS PAT clone.

**`create_source`:** returns a `GiteaSource` (Decision 2a).

**`mint_scoped_repo_token`:** returns `Err(NotImplemented)`. The credential
daemon falls back to the stored long-lived PAT (same posture as GitLab PAT
connections).

#### 1b. `BitbucketProvider` — `crates/temps-git/src/services/bitbucket_provider.rs`

**Cloud vs Data Center scope cut (v1):**

- **Bitbucket Cloud** (`bitbucket.org`): REST API v2.0 at `api.bitbucket.org/2.0`.
  Fixed host; `base_url` is always `https://bitbucket.org` and `api_url` is
  always `https://api.bitbucket.org/2.0`. v1 supports this path.
- **Bitbucket Data Center / Server**: entirely different REST API at
  `/rest/api/1.0`, different auth headers, different webhook shape. v1 does NOT
  support Data Center. The UI must detect when a user enters a non-bitbucket.org
  URL and guide them to the Manual/Generic tier instead.

`BitbucketProvider::new()` takes no user-supplied `base_url` (Cloud is a fixed
host) and `auth_method: AuthMethod`. The API base is the constant string
`https://api.bitbucket.org/2.0`.

**Authentication:** Bitbucket Cloud supports Repository/Workspace Access Tokens
(RATs/WATs) and App Passwords. For v1:

- RATs/WATs: use `AuthMethod::PersonalAccessToken { token }` with HTTP Basic
  username `x-token-auth`. This is Bitbucket Cloud's documented convention for
  access tokens over HTTPS.
- App Passwords: use `AuthMethod::BasicAuth { username, password }` where
  `username` is the Atlassian account username.

**Key API mappings (Bitbucket Cloud v2.0):**

| Operation | Bitbucket API endpoint |
|---|---|
| Get user | `GET /2.0/user` |
| List repos | `GET /2.0/repositories/{workspace}?pagelen=50&page=N` |
| Get repo | `GET /2.0/repositories/{workspace}/{repo_slug}` |
| List branches | `GET /2.0/repositories/{workspace}/{repo_slug}/refs/branches` |
| List tags | `GET /2.0/repositories/{workspace}/{repo_slug}/refs/tags` |
| Get file content | `GET /2.0/repositories/{workspace}/{repo_slug}/src/{commit}/{path}` |
| Get commit | `GET /2.0/repositories/{workspace}/{repo_slug}/commit/{sha}` |
| List commits | `GET /2.0/repositories/{workspace}/{repo_slug}/commits/{branch}?pagelen={n}` |
| Create webhook | `POST /2.0/repositories/{workspace}/{repo_slug}/hooks` |
| Delete webhook | `DELETE /2.0/repositories/{workspace}/{repo_slug}/hooks/{uid}` |
| PR comment | `POST /2.0/repositories/{workspace}/{repo_slug}/pullrequests/{id}/comments` |
| Create repo | `POST /2.0/repositories/{workspace}/{repo_slug}` |
| Create PR | `POST /2.0/repositories/{workspace}/{repo_slug}/pullrequests` |

**Pagination:** Bitbucket Cloud uses cursor-based pagination with a `next` URL
in the JSON response body `values[].next` — not Link headers. Override
`list_repositories_page` to follow the cursor.

**Webhook signature — the critical difference from GitHub and Gitea:**

Bitbucket Cloud webhooks do NOT include an HMAC body signature by default. The
platform provides `X-Event-Key` (event type) and `X-Hook-UUID` (idempotency
identifier), but there is no `X-Hub-Signature-256` equivalent and no
configurable HMAC secret on the hook.

**Decision: secret-in-path URL token.** At webhook installation time, Temps
generates a cryptographically random 64-character hex token (32 bytes from
`rand::rngs::OsRng`, matching the existing `gitlab_webhook.rs` pattern — NOT
`thread_rng`; **MUST-FIX 1**). The hook is registered pointing at:

```
{temps_external_url}/api/webhook/git/bitbucket/events/{delivery_token}
```

The handler extracts `{delivery_token}` from the path and looks up the project.
The lookup must fetch candidate rows and compare the decrypted token in
**constant time** (`subtle::ConstantTimeEq`, already in the lockfile), never via
SQL equality on ciphertext; and the handler must **always return HTTP 200**
regardless of whether the token matches or a project is found (no existence
oracle) — **MUST-FIX 3**. The token-bearing request path must never be logged.
The token is stored encrypted and never returned in plaintext via the API.

This approach is used by comparable integrations. It is weaker than HMAC
because a replayed captured request succeeds (no per-message MAC), but it is
the mechanism Bitbucket Cloud offers, and the `security-auditor` signed off on
it (Section 14).

**Auto-webhook registration (in scope).** On project connect, Temps generates
the per-project `bitbucket_webhook_token` (if absent) and calls `create_webhook`
to register the hook pointing at the secret-in-path URL; disconnect calls
`delete_webhook`. If auto-registration fails, the connect does not hard-fail —
the webhook URL + token are surfaced in the UI as a manual-config fallback.
Events handled: `repo:push` (deploy trigger) and `pullrequest:created` /
`pullrequest:updated` (PR preview + comment status).

**PR commenting (in scope).** Find-or-update a Temps bot comment via
`GET`/`POST`/`PUT .../pullrequests/{id}/comments`, reusing the shared
comment-body format and the `<!-- temps-preview:... -->` marker convention.

**Clone username:** `x-token-auth` for RATs/WATs; the user's Atlassian username
for App Passwords (from `AuthMethod::BasicAuth`).

**`create_source`:** returns a `BitbucketSource` (Decision 2b).

#### 1c. `GenericProvider` (Manual Git Providers) — `crates/temps-git/src/services/generic_provider.rs`

The Generic provider is the "Other Git Providers" tier in the sidebar. It
targets Azure DevOps, SourceHut, Gogs, AWS CodeCommit, Gitblit, Rhodecode,
Forgejo instances where the operator doesn't want a full native integration,
and any other HTTPS-cloneable git server. It is the intentional acceptance of
the approach previously rejected for Gitea (native API required), now applied
as the complementary universal fallback tier.

**Two connection modes (HTTPS only):**

**Mode A — Public repository (no credentials):** provide a clone URL only. The
handler stores `clone_url` in connection metadata and calls
`git_ops::clone_repo(url, target_dir, branch)` directly. No credentials
involved.

**Mode B — Private via HTTPS token:** provide a clone URL, a token, and a
user-supplied clone username. Uses `AuthMethod::BasicAuth { username, password
}` (where `password` is the token) or `AuthMethod::PersonalAccessToken {
token }` (username defaults to `x-access-token`). Clone uses
`git_ops::clone_repo_with_credentials(url, dir, username, token, branch)`.
This covers Azure DevOps PATs (username `org@org.com`), AWS CodeCommit
(special HTTPS credentials), and most other HTTPS-authenticated git hosts.

**What `GenericProvider` implements:**

- `authenticate` / `get_auth_url`: `Err(NotImplemented)`
- `validate_token`: attempts an unauthenticated HEAD request on the clone URL
  for Mode A; returns `Ok(true)` for token modes (no generic user API). Health
  records as `"unknown"` with a `health_message` explaining that Generic
  providers do not support active health checking — a public clone URL being
  reachable doesn't mean a deploy will succeed.
- `token_needs_refresh` / `validate_and_refresh_token`: always `false` / token
  unchanged
- `get_user`: `Err(NotImplemented)`
- `list_repositories` / `get_repository` / `list_branches` / etc.: all
  `Err(NotImplemented)`. The repository picker shows an empty state with a
  tooltip explaining the tier limitation.
- `create_webhook` / `delete_webhook` / `verify_webhook_signature`:
  `Err(NotImplemented)`. See Decision 4 for the Generic webhook handler
  (manual configuration, secret-in-path).
- `clone_repository`: dispatches by auth method — Mode A via `clone_repo`,
  Mode B via `clone_repo_with_credentials`.
- `download_archive`: `Err(NotImplemented)` — the deployer falls back to clone
- `create_source`: `Err(NotImplemented)` — framework detection requires clone
- `mint_scoped_repo_token`: `Err(NotImplemented)`

**Connection creation:** a `create_generic_provider` helper in
`GitProviderManager` stores `clone_url` and `token_username` in the
connection's `metadata: Option<Json>` column:
`{"clone_url": "...", "token_username": "..."}`. The `clone_url` is validated
with `validate_git_url` (HTTPS-only) at create time.

### 2. Three new `ProjectSource` implementations

#### 2a. `GiteaSource` — `crates/temps-git/src/sources/gitea_source.rs`

Implements `ProjectSource` via on-demand
`GET /api/v1/repos/{owner}/{repo}/contents/{path}?ref={ref}` calls.
Holds `client: Arc<reqwest::Client>`, `base_url`, `owner`, `repo`,
`reference`, `access_token`. Auth: `Authorization: token {pat}`.

#### 2b. `BitbucketSource` — `crates/temps-git/src/sources/bitbucket_source.rs`

Implements `ProjectSource` via
`GET /2.0/repositories/{workspace}/{repo}/src/{commit}/{path}`.
Bitbucket Cloud returns file content directly, not base64. Auth: `Bearer
{token}` for RATs/WATs; Basic auth for App Passwords.

#### 2c. No `GenericSource`

Framework detection for Manual/Generic providers requires cloning first.
`GenericProvider::create_source` returns `Err(NotImplemented)`. The deployer
falls back to clone-then-detect.

### 3. Manual tier is HTTPS-only (no SSH)

The Manual/Generic tier supports public-repo and HTTPS-token clone only. SSH
deploy keys — the Coolify/Dokploy headline flow — are explicitly **out of
scope** for this ADR.

Rationale: HTTPS + token already covers GitHub, GitLab, Gitea, Bitbucket Cloud,
Azure DevOps, AWS CodeCommit, and the large majority of self-hosted hosts, so
the tier is fully useful over HTTPS. SSH support would add a meaningful,
security-sensitive surface — server-side keypair generation, host-key
verification (TOFU vs pinning), private-key-at-rest handling, libgit2 SSH
plumbing — that is not justified by the v1 use cases. It can be revisited as a
separate ADR if real demand appears; the `AuthMethod::SSHKey` enum variant
already exists in the type system, so a future addition needs no schema change.

### 4. Handler files and route registration

| Provider | Handler file | Route path(s) |
|---|---|---|
| Gitea | `handlers/gitea.rs` | `POST /webhook/git/gitea/events` |
| Bitbucket | `handlers/bitbucket.rs` | `POST /webhook/git/bitbucket/events/{delivery_token}` |
| Generic | `handlers/generic.rs` | `POST /webhook/git/generic/events/{delivery_token}` |

All three `configure_routes()` functions are merged in the handler mod file.

**Gitea handler:** extracts `X-Gitea-Event` header, reads `X-Gitea-Signature`
for HMAC-SHA256 verification, dispatches push events to the existing
`handle_push_event` code path. Projects with no stored `gitea_webhook_signing_token`
are rejected following the `project_has_signing_token` pattern from
`gitlab.rs:47–52`.

**Bitbucket handler:** extracts `X-Event-Key`. Validates `{delivery_token}`
against `projects.bitbucket_webhook_token` (decrypted, constant-time).
Dispatches `repo:push` and `pullrequest:created`/`pullrequest:updated`; logs +
discards other events.

**Generic handler:** validates `{delivery_token}` against
`projects.generic_webhook_token`, accepts any JSON body with a `ref` field,
dispatches a deploy.

**Required hardening for all three handlers (security review):** apply
`DefaultBodyLimit::max(512 * 1024)` at route registration — webhook bodies are
a few KB, and the git handlers currently have no body limit (**MUST-FIX 2**;
mirror the sentry-ingest pattern). Verify the HMAC / secret-in-path token
on the **raw bytes before JSON parsing**. Return **HTTP 200 on
signature/token failure**, matching the existing GitLab handler
(`gitlab.rs:285`) — do NOT copy GitHub's 401, which leaks that a signature was
checked. Log `X-Gitea-Delivery` / `X-Hook-UUID` for traceability, never the
token-bearing URL path.

### 5. Self-hosted base URL: storage and SSRF

| Provider | `base_url` | `api_url` |
|---|---|---|
| Gitea | Mandatory. Instance web root. | Derived: `{base_url}/api/v1` |
| Bitbucket Cloud | Fixed: `https://bitbucket.org` | Fixed: `https://api.bitbucket.org/2.0` |
| Generic | Optional, display only. | Not used. |

The Gitea `base_url` and the Generic `clone_url` must be validated with the
**HTTPS-only `validate_git_url`**, NOT `validate_external_url` — the latter
permits plaintext `http://`, which would transmit the PAT in cleartext
(**MUST-FIX 4**). Validation must run both at create time (handler) and inside
`clone_repository` before invoking `git_ops` (so a later `metadata` edit cannot
bypass it). Bitbucket Cloud's fixed HTTPS `api_url` needs no per-request check.
Both validators still block loopback, RFC1918, link-local, and cloud-metadata
addresses (verified in `temps-core/src/url_validation.rs`).

**Residual risk:** neither validator resolves hostnames. DNS rebinding is
unmitigated (an async `validate_domain_async` exists in `temps-core` but is
unused). Pre-existing for self-hosted GitLab; accepted for v1 and tracked
separately as a workspace-wide item.

### 6. Auth model: PAT-first, OAuth deferred for all three

None of the three new providers support OAuth in v1:

- **Gitea:** per-instance app registration, no central store. PAT universally
  available. `start_oauth_flow` returns a user-visible `InvalidConfiguration`
  error directing to PAT instead.
- **Bitbucket Cloud:** per-workspace app registration. RATs/WATs and App
  Passwords cover the v1 use case. Deferred to v2.
- **Generic:** no OAuth by definition.

The `start_oauth_flow` match in `git_provider_manager.rs:2192` gains three
arms returning `InvalidConfiguration` with provider-specific messages
(Gitea points to Settings > Applications > Access Tokens; Bitbucket points to
Repository Settings > Access tokens; Generic explains the tier limitation).

### 7. `GitProviderFactory` wiring

```rust
GitProviderType::Gitea => {
    use crate::services::gitea_provider::GiteaProvider;
    let base_url = base_url.ok_or_else(|| GitProviderError::InvalidConfiguration(
        "Gitea provider requires a base_url (e.g. https://git.example.com)".to_string(),
    ))?;
    Ok(Box::new(GiteaProvider::new(base_url, auth_method)))
}
GitProviderType::Bitbucket => {
    use crate::services::bitbucket_provider::BitbucketProvider;
    Ok(Box::new(BitbucketProvider::new(auth_method)))
}
GitProviderType::Generic => {
    use crate::services::generic_provider::GenericProvider;
    Ok(Box::new(GenericProvider::new(base_url, auth_method)))
}
```

### 8. `GitPrCommenter` extension

Add `"gitea"` and `"bitbucket"` arms to the match in
`pr_commenter.rs::upsert_inner` (line 180). Gitea issue comments use
`POST /api/v1/repos/{owner}/{repo}/issues/{number}/comments` (PRs are
represented as issues); Bitbucket uses
`POST /2.0/repositories/{workspace}/{repo_slug}/pullrequests/{id}/comments`.
No Generic arm (the Manual tier has no API).

### 9. Credential daemon username conventions

Extend the match in `git_provider_manager.rs::mint_scoped_repo_token_for_connection`
(line 777–779):

```rust
let username = match provider.provider_type.as_str() {
    "gitlab"    => "oauth2",
    "gitea"     => "x-access-token",
    "bitbucket" => "x-token-auth",
    _           => "x-access-token",
}.to_string();
```

### 10. Connection health

- **GiteaProvider:** `GET /api/v1/user` -> `Ok(true)` on 200, `Ok(false)` on 401
- **BitbucketProvider:** `GET /2.0/user` -> same pattern
- **GenericProvider (all modes):** records `health_status = "unknown"` with
  `health_message = "Health checking is not available for Manual git
  providers"`. The UI displays `unknown` as a distinct badge, not `healthy`.

### 11. Data model changes

No new columns in `git_providers` or `git_provider_connections`. Three new
nullable columns added to the `projects` table, one webhook token per provider
type.

```sql
-- New migration:
-- crates/temps-migrations/src/migration/m20260701_000001_add_provider_webhook_tokens.rs

ALTER TABLE projects
    ADD COLUMN gitea_webhook_signing_token TEXT,  -- HMAC key, encrypted at rest
    ADD COLUMN bitbucket_webhook_token     TEXT,  -- secret-in-path token, encrypted
    ADD COLUMN generic_webhook_token       TEXT;  -- secret-in-path token, encrypted
```

Entity update: `crates/temps-entities/src/projects.rs` — add three
`Option<String>` fields, each carrying `#[serde(skip_serializing)]` (matching
`gitlab_webhook_signing_token` at `projects.rs:69`) so the encrypted tokens
never appear in a serialized `Model` response (**MUST-FIX 5**).

This migration is additive-only (nullable columns, no renames), consistent with
the schema-skew discipline from ADR-017.

### 12. Cargo dependencies

**This ADR adds no new Cargo dependencies.** Gitea, Bitbucket, and the
HTTPS/public Manual modes reuse `reqwest`, `hmac`, `sha2`, `git2`, `rand`,
`subtle`, and `temps_presets::source::ProjectSource`, all already in the
workspace.

### 13. Scope: v1 vs deferred

**In scope for v1:**

- `GiteaProvider` — all `GitProviderService` trait methods (PAT auth)
- `GiteaSource` — `ProjectSource` (framework detection)
- Gitea PAT connection creation (`create_gitea_pat_provider`)
- Gitea repository sync, archive download, SSRF-safe archive redirect guard
- Gitea webhook installation + `X-Gitea-Signature` HMAC verification + push handler
- Gitea PR commenting (`GitPrCommenter` new arm)
- `BitbucketProvider` — Cloud only, PAT/App Password auth
- `BitbucketSource` — `ProjectSource` (framework detection)
- Bitbucket Cloud connection creation, repository sync
- Bitbucket auto-webhook registration (secret-in-path) + push/PR event handling
- Bitbucket PR commenting (`GitPrCommenter` new arm)
- `GenericProvider` — two modes: public repo + HTTPS token
- Generic webhook URL surfaced in UI; handler with secret-in-path validation
- Three new `projects` columns + migration
- No new Cargo dependencies
- `security-auditor` sign-off (obtained 2026-06-30)

**Deferred to v2 / out of scope:**

- SSH deploy keys / SSH auth for the Manual tier (Decision 3 — out of scope)
- Gitea OAuth 2.0
- Gitea org-level webhook installation (uses repo-level only)
- Bitbucket Cloud OAuth 2.0
- Bitbucket Data Center / Server (different API; route to Generic tier)
- `GenericSource` for framework detection without clone

### 14. Security requirements

**S1: SSRF via user-supplied base URL (Gitea, Generic HTTPS clone URL)**
Mitigated by the HTTPS-only `validate_git_url` (MUST-FIX 4). DNS rebinding is a
residual unmitigated risk (tracked separately).

**S2: Webhook authenticity**

- Gitea: HMAC-SHA256 (`X-Gitea-Signature`). Replay risk accepted (same posture
  as GitHub). Projects with no stored signing token are rejected.
- Bitbucket: secret-in-path URL token (no HMAC). A captured delivery URL can
  be replayed. Weaker than HMAC, but it is the mechanism Bitbucket Cloud
  offers; signed off by `security-auditor`.
- Generic: secret-in-path URL token. Same risk profile as Bitbucket.

**S3: Token and key storage**
All PATs, App Passwords, and webhook tokens are stored encrypted via
`EncryptionService` (AES-256-GCM). API responses mask secrets as `***`.

### Security review outcome (2026-06-30)

**Verdict: SIGN-OFF WITH CONDITIONS.** The `security-auditor` verified the
design against the real codebase (`url_validation.rs`, `encryption.rs`,
`handlers/{github,gitlab}.rs`, `gitlab_webhook.rs`, `projects.rs`) and approved
it subject to five MUST-FIX conditions, all folded into the decisions above:

1. **MUST-FIX 1** — generate the Bitbucket/Generic webhook tokens with
   `rand::rngs::OsRng`, not `thread_rng` (Decision 1b).
2. **MUST-FIX 2** — apply `DefaultBodyLimit::max(512 KiB)` to all three webhook
   handlers; the git handlers have no body limit today (Decision 4).
3. **MUST-FIX 3** — the secret-in-path handlers must return HTTP 200 on any
   failure (no existence oracle) and compare the decrypted token in constant
   time, never via SQL equality on ciphertext (Decision 1b, 4).
4. **MUST-FIX 4** — validate Gitea `base_url` and Generic `clone_url` with the
   HTTPS-only `validate_git_url`, at both create time and clone time;
   `validate_external_url` permits plaintext HTTP and would leak the PAT
   (Decision 5).
5. **MUST-FIX 5** — the three new `projects` token fields must carry
   `#[serde(skip_serializing)]` (Decision 11).

**Accepted residual risks:** DNS-rebinding SSRF (pre-existing for self-hosted
GitLab), Gitea/GitHub HMAC replay (no timestamp), and Bitbucket secret-in-path
replay (inherent to the mechanism Bitbucket offers).

**Deferred follow-on (non-blocking):** webhook-token rotation endpoint for
Bitbucket/Generic, optional source-IP allowlist, and an audit-log entry on
token generation.

This is a **design** sign-off. Items 1–5 plus the constant-time lookup, the
clone-time URL re-validation, the body-limit layers, and "no full-path logging"
must be re-verified at PR review.

## Consequences

### Positive

- Gitea and Forgejo users can connect their self-hosted instance and deploy
  from it without mirroring to a hosted provider.
- Bitbucket Cloud gets the full experience: repo picker, auto-webhook, push
  deploys, and PR comments.
- Any git server cloneable over HTTPS (Azure DevOps, AWS CodeCommit, SourceHut,
  Gogs, Gitblit, private mirrors) is deployable via the Manual tier.
- No new Cargo dependencies and no native-library changes — everything reuses
  what's already vendored.
- No new columns in `git_providers` or `git_provider_connections`. Three new
  nullable columns on `projects` only.

### Negative

- The Manual tier is HTTPS-only; hosts that require SSH (or operators who prefer
  read-only deploy keys) are not served. Mitigated by fine-grained / repo-scoped
  PATs, which most providers support.
- Bitbucket and Generic use secret-in-path webhook authentication (weaker than
  HMAC). Accepted as the mechanism each platform offers, but a conscious
  downgrade from the GitHub/GitLab/Gitea HMAC posture.

### Neutral

- Forgejo is API-compatible with all Gitea methods. Works without special
  casing.
- The prior "Option A: generic git server" previously rejected as a Gitea
  substitute is now the explicitly adopted Manual/Generic tier — complementary,
  not competitive.
- OAuth for Gitea and Bitbucket uses existing `AuthMethod::OAuth` and the
  existing `start_oauth_flow`/`handle_oauth_callback` machinery. Adding v2
  OAuth requires only new match arms; no schema migration.

### Risks

- **Gitea API version divergence:** instances older than Gitea 1.12 may be
  missing `repos/search`. Detect 404 and fall back to `user/repos`.
- **Bitbucket Cloud API rate limits:** the v2.0 API enforces per-IP and
  per-user rate limits. The repository sync loop must respect
  `X-RateLimit-Remaining` and back off when near the limit.
- **Webhook secret rotation:** if a Gitea hook HMAC secret is changed without
  updating the Temps DB row, deliveries fail silently (returns 200 to avoid
  leaking existence). Operators need a documented re-enrollment flow.

## Alternatives Considered

### Option A: Native Gitea + Generic/Manual only, no Bitbucket

Defer Bitbucket and route Bitbucket Cloud users to the Manual/Generic tier.

Rejected: Bitbucket appears by name in the sidebar IA. Users expect a
first-class connection with a repository picker. A Generic connection cannot
provide that.

### Option B: HTTPS-only for the Manual tier (ADOPTED)

Ship the Manual tier with public-repo and HTTPS-token modes only; do not
implement SSH deploy keys.

**Adopted.** HTTPS + token covers GitHub, GitLab, Gitea, Bitbucket Cloud, Azure
DevOps, and most self-hosted hosts, so the tier is fully useful over HTTPS. SSH
deploy keys would add a meaningful, security-sensitive surface (server-side
keygen, host-key verification, private-key-at-rest, libgit2 SSH plumbing) not
justified by the v1 use cases. If real demand appears, SSH can be a separate
ADR — and because `AuthMethod::SSHKey` already exists, it would need no schema
change.

### Option C: Single `provider_webhook_token` column on `projects`

Use one nullable column for all provider webhook tokens rather than three.

Rejected: the HMAC key (Gitea) and URL path tokens (Bitbucket, Generic) have
different security models. Conflating them in one column risks a handler bug
using the wrong token type. Separate named columns make the security boundary
explicit at the schema level and match the existing `gitlab_webhook_signing_token`
precedent.

## Implementation Notes

- **Affected crates:**
  - `crates/temps-git/src/services/gitea_provider.rs` (new file)
  - `crates/temps-git/src/services/bitbucket_provider.rs` (new file)
  - `crates/temps-git/src/services/generic_provider.rs` (new file)
  - `crates/temps-git/src/sources/gitea_source.rs` (new file)
  - `crates/temps-git/src/sources/bitbucket_source.rs` (new file)
  - `crates/temps-git/src/sources/mod.rs` (register new sources)
  - `crates/temps-git/src/handlers/gitea.rs` (new file)
  - `crates/temps-git/src/handlers/bitbucket.rs` (new file)
  - `crates/temps-git/src/handlers/generic.rs` (new file)
  - `crates/temps-git/src/handlers/mod.rs` (merge new routes)
  - `crates/temps-git/src/services/git_provider.rs` (wire three factory arms)
  - `crates/temps-git/src/services/git_provider_manager.rs` (add
    `create_gitea_pat_provider`, `create_bitbucket_provider`,
    `create_generic_provider`; extend `start_oauth_flow` and
    `mint_scoped_repo_token_for_connection`)
  - `crates/temps-git/src/services/pr_commenter.rs` (add `"gitea"` + `"bitbucket"` arms)
  - `crates/temps-entities/src/projects.rs` (three new fields)
  - `crates/temps-migrations/src/migration/m20260701_000001_add_provider_webhook_tokens.rs` (new)
  - `crates/temps-migrations/src/migration/mod.rs` (register migration)
- **Migration needed:** yes — three nullable `TEXT` columns on `projects`
- **New Cargo dependency:** none
- **Breaking changes:** no
- **Security auditor sign-off:** OBTAINED (2026-06-30) — SIGN-OFF WITH
  CONDITIONS. Five MUST-FIX items (see Section 14 "Security review outcome")
  must be implemented; each is also marked inline in the relevant decision.
  Re-verify at PR review: constant-time token lookup, clone-time URL
  re-validation (`validate_git_url`), `DefaultBodyLimit` on all three handlers,
  `#[serde(skip_serializing)]` on the new fields, HTTP-200-on-failure, and no
  token-bearing path in any log line.
