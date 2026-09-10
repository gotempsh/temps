// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * API key management lifecycle against a live Temps instance -- proving the
 * actual HTTP-level round trip (create -> scoped 200/403 -> revoke -> cut
 * off), not just that the console wizard can mint one.
 *
 * `web/e2e/authenticated/api-key-create.spec.ts` already covers the
 * browser-only path: open the 3-step wizard, pick the first predefined
 * role, submit, and confirm the returned secret authenticates a single
 * `GET /projects` call. That is real coverage but narrow -- it never tries
 * a *scoped* (non-predefined-role) key, never checks that the scope is
 * actually enforced as a ceiling (200 inside it, 403 outside it), and never
 * revokes anything. This scenario is deliberately the complement: it skips
 * the UI and drives `POST /api-keys` -> use -> revoke -> use-again entirely
 * through the real API, the same way `rbac-scenario.ts` complements the
 * team/access UI.
 *
 * Real constraint discovered while building this (not a bug -- a deliberate
 * anti-privilege-escalation boundary, see
 * `crates/temps-auth/src/sensitive_action.rs`'s `DefaultSensitiveActionAuthorizer`):
 * `POST /api-keys` is gated behind `SensitiveAction::CreateApiKey`, and the
 * default policy DENIES every principal that isn't an interactive
 * `UserSession` -- API keys, CLI tokens, and deployment tokens are all
 * machine principals and get `Deny { reason: "interactive_verification_required" }`
 * outright (not even the 428 step-up flow; see
 * `machine_principals_are_denied_by_default` in that file's own tests).
 * Since every SDK client `apps/temps-e2e` builds via `makeClient()` is
 * Bearer-API-key-authenticated (see `lib/client.ts`), the PRIMARY e2e
 * identity can never itself call `POST /api-keys` -- exactly the same wall
 * `rbac-scenario.ts` hit when it needed a second user's OWN bearer key
 * (its comment: "minting a NEW key while already authenticated via API key
 * is deliberately blocked"). rbac-scenario routes around this by minting
 * DB-direct; that shortcut is unusable HERE because the entire point of
 * this scenario is to prove the real `POST /api-keys` HTTP endpoint works.
 *
 * The only principal type the default policy allows through is a real
 * interactive browser session, so this scenario creates a throwaway
 * low-privilege second user (via the CLI, same as rbac-scenario), logs it
 * in for real against `POST /auth/login`, and uses the resulting session
 * cookie -- NOT a bearer token -- to drive key creation and revocation.
 * Two more things had to be confirmed by reading the actual production
 * auth path rather than assuming from names:
 *
 *   - The session cookie is named plain `session` (see
 *     `AuthService::create_session_cookie` and
 *     `AuthMiddleware::extract_user_session_from_cookies`, the middleware
 *     actually wired into the router via the plugin system). A same-named
 *     `SESSION_ID_COOKIE_NAME = "_temps_sid"` constant exists in
 *     `crates/temps-auth/src/middleware.rs`, but that file's
 *     `auth_middleware` free function is NOT the one registered as
 *     `TempsMiddleware` -- `_temps_sid` is actually the visitor/session
 *     *tracking* cookie a Temps-deployed customer app reads (see
 *     `packages/api` -- `X-Temps-...` / `_temps_visitor_id` analytics
 *     docs), an unrelated feature that happens to reuse the string
 *     "session". Sending `Cookie: _temps_sid=...` to the console API does
 *     nothing; only `Cookie: session=...` authenticates.
 *   - The cookie-vs-bearer branches are mutually exclusive at the
 *     middleware level: an `Authorization` header of ANY kind (even one
 *     that fails to parse as `tk_`/`dt_`) skips the cookie-session branch
 *     entirely (see the `if let Some(auth_header) = ... else { try cookie }`
 *     shape in `execute_auth_middleware_logic`). So the login + create/
 *     revoke calls below are raw `fetch()`, never the SDK client (which
 *     unconditionally attaches a Bearer header via its request
 *     interceptor).
 *
 * Revocation immediacy: `ApiKeyService::validate_api_key` (the function the
 * auth middleware calls on every `tk_...`-bearing request) queries
 * `api_keys` directly with `IsActive.eq(true)` on every single call -- no
 * cache, no TTL, nothing in between the DB row and the auth decision (the
 * only caching in this service, `LastUsedThrottle`, only throttles the
 * `last_used_at` write-back, never the read that decides pass/fail). So the
 * revoke -> immediate-retry assertion below is a single request, not a
 * poll -- confirmed by reading the query, not assumed from a doc comment.
 */
import { getProject, updateProject, deleteUser } from '@temps-sdk/api'
import { makeClient, normalizeApiUrl, resolveConfig, unwrap } from '../lib/client.ts'
import { createE2eProject, teardown, makeRunId } from '../lib/flows.ts'
import { runCliOk, runCliJson } from '../lib/cli-exec.ts'

export interface ApiKeyScenarioOptions {
  tempsRoot?: string
  databaseUrl?: string
  keep?: boolean
  json?: boolean
  connection: { url?: string; apiKey?: string }
}

interface StepLog {
  step: string
  ok: boolean
  detail?: string
  ms?: number
}

interface ApiKeyScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
}

interface CreateApiKeyHttpResponse {
  id: number
  name: string
  key_prefix: string
  role_type: string
  permissions: string[] | null
  api_key: string
  expires_at: string | null
  created_at: string
}

/** Extract the `session=...` cookie value pair from a Set-Cookie response header set. */
function extractSessionCookie(setCookieValues: string[]): string {
  for (const raw of setCookieValues) {
    // Each Set-Cookie header value looks like `session=<value>; Path=/; ...`
    const match = /^session=([^;]+)/.exec(raw.trim())
    if (match) return `session=${match[1]}`
  }
  throw new Error(
    `no "session" cookie found in login response Set-Cookie headers: ${JSON.stringify(setCookieValues)}`,
  )
}

/**
 * Bun's `fetch` Response exposes multiple Set-Cookie header values via
 * `headers.getSetCookie()` (Node/Bun-specific -- the raw Fetch spec's
 * `Headers.get('set-cookie')` folds them into one comma-joined string,
 * which would corrupt cookie parsing since cookie attributes themselves
 * contain commas in `Expires=`). Falls back to a single `get()` call for
 * environments where `getSetCookie` isn't available.
 */
function getSetCookieValues(headers: Headers): string[] {
  const withGetSetCookie = headers as Headers & { getSetCookie?: () => string[] }
  if (typeof withGetSetCookie.getSetCookie === 'function') {
    return withGetSetCookie.getSetCookie()
  }
  const single = headers.get('set-cookie')
  return single ? [single] : []
}

export async function apiKeyScenarioCommand(opts: ApiKeyScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps API key scenario  ->  ${cfg.url}`)

  const tempsRoot = opts.tempsRoot ?? process.env.TEMPS_ROOT
  const databaseUrl = opts.databaseUrl ?? process.env.TEMPS_DATABASE_URL
  if (!tempsRoot || !databaseUrl) {
    throw new Error(
      'API key testing needs a real interactive login (POST /api-keys denies every machine ' +
        'principal by default, including the bearer key this harness itself uses) -- pass ' +
        '--temps-root <checkout> --database-url <postgres url>, or set TEMPS_ROOT / ' +
        'TEMPS_DATABASE_URL, so a throwaway user can be created via the CLI. See ' +
        'apps/temps-e2e/README.md.',
    )
  }

  const apiBase = normalizeApiUrl(cfg.url)

  const runId = makeRunId(Date.now())
  const steps: StepLog[] = []
  const step = async <T>(name: string, fn: () => Promise<T>): Promise<T> => {
    const t0 = performance.now()
    log(`\n▶ ${name}`)
    try {
      const r = await fn()
      const ms = performance.now() - t0
      steps.push({ step: name, ok: true, ms })
      log(`  ✓ ${name} (${(ms / 1000).toFixed(1)}s)`)
      return r
    } catch (e) {
      const ms = performance.now() - t0
      steps.push({ step: name, ok: false, detail: (e as Error).message, ms })
      log(`  ✗ ${name}: ${(e as Error).message}`)
      throw e
    }
  }

  const secondUsername = `${runId}-keyuser`.replace(/[^a-z0-9-]/gi, '-').slice(0, 32)
  const secondEmail = `${runId}-second@e2e.test`
  const secondPassword = `E2e!${runId}Aa1`
  const keyName = `${runId}-scoped-key`

  const projectIds: number[] = []
  let secondUserId: number | undefined
  let sessionCookie: string | undefined
  let scopedApiKeyId: number | undefined
  let scopedApiKey: string | undefined
  let scopedApiKeyRevoked = false

  try {
    const project = await step('create a throwaway project (primary bearer identity)', () =>
      createE2eProject(client, { name: `${runId}-apikey-app` }),
    )
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    await step('create a low-privilege second user (role: user, via CLI)', () =>
      runCliOk(
        ['users', 'create', '-u', secondUsername, '-e', secondEmail, '-p', secondPassword, '-r', 'user', '-y'],
        cfg,
      ),
    )

    secondUserId = await step("resolve the second user's id (users create has no --json output)", async () => {
      const users = await runCliJson<Array<{ user: { id: number; email: string } }>>(['users', 'list'], cfg)
      const match = users.find((u) => u.user.email === secondEmail)
      if (!match) throw new Error(`created user ${secondEmail} not found in users list`)
      return match.user.id
    })
    log(`  second user #${secondUserId} (${secondEmail})`)

    sessionCookie = await step(
      'log the second user in for real (POST /auth/login) -- the only principal type POST /api-keys accepts',
      async () => {
        const res = await fetch(`${apiBase}/auth/login`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ email: secondEmail, password: secondPassword }),
        })
        if (!res.ok) {
          throw new Error(`POST /auth/login failed: HTTP ${res.status} ${await res.text()}`)
        }
        const setCookies = getSetCookieValues(res.headers)
        return extractSessionCookie(setCookies)
      },
    )

    const created = await step(
      'POST /api-keys (session-cookie auth): a custom key scoped to ONLY projects:read',
      async () => {
        const res = await fetch(`${apiBase}/api-keys`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json', Cookie: sessionCookie! },
          body: JSON.stringify({
            name: keyName,
            role_type: 'custom',
            permissions: ['projects:read'],
            expires_at: null,
          }),
        })
        if (res.status !== 201) {
          throw new Error(`POST /api-keys failed: HTTP ${res.status} ${await res.text()}`)
        }
        const body = (await res.json()) as CreateApiKeyHttpResponse
        if (!body.api_key || !body.api_key.startsWith('tk_')) {
          const preview = body.api_key ? `${body.api_key.slice(0, 6)}...` : '(missing)'
          throw new Error(
            `POST /api-keys returned api_key ${preview}, expected a 'tk_'-prefixed plaintext secret`,
          )
        }
        return body
      },
    )
    scopedApiKeyId = created.id
    scopedApiKey = created.api_key
    log(`  scoped key #${created.id} (${created.key_prefix}...)`)

    await step('scoped key: in-scope read succeeds (200, real project fields)', async () => {
      const scopedClient = makeClient({ url: cfg.url, apiKey: scopedApiKey! })
      const read = unwrap(await getProject({ client: scopedClient, path: { id: project.id } }), 'getProject')
      if (read.id !== project.id) {
        throw new Error(`GET /projects/${project.id} with scoped key returned id=${read.id}`)
      }
    })

    await step('scoped key: out-of-scope write is 403 naming projects:write', async () => {
      const scopedClient = makeClient({ url: cfg.url, apiKey: scopedApiKey! })
      const write = await updateProject({
        client: scopedClient,
        path: { id: project.id },
        body: {
          name: `${project.name}-renamed`,
          directory: '.',
          main_branch: 'main',
          preset: 'dockerfile',
          storage_service_ids: [],
          source_type: 'docker_image',
          is_web_app: true,
        },
      })
      if (!write.error || write.response?.status !== 403) {
        throw new Error(`PATCH with a projects:read-only key returned HTTP ${write.response?.status}, expected 403`)
      }
      const body = write.error as { required_permission?: string } | undefined
      if (body?.required_permission !== 'projects:write') {
        throw new Error(
          `PATCH-with-scoped-key 403 body.required_permission="${body?.required_permission}", expected "projects:write"`,
        )
      }
    })

    await step('revoke the key (POST /api-keys/{id}/deactivate, session-cookie auth)', async () => {
      const res = await fetch(`${apiBase}/api-keys/${scopedApiKeyId}/deactivate`, {
        method: 'POST',
        headers: { Cookie: sessionCookie! },
      })
      if (!res.ok) {
        throw new Error(`POST /api-keys/${scopedApiKeyId}/deactivate failed: HTTP ${res.status} ${await res.text()}`)
      }
      const body = (await res.json()) as { is_active: boolean }
      if (body.is_active !== false) {
        throw new Error(`deactivate response reports is_active=${body.is_active}, expected false`)
      }
      scopedApiKeyRevoked = true
    })

    await step(
      'revoked key: the VERY NEXT request with it is rejected (401) -- validate_api_key reads is_active straight ' +
        'from the DB on every call, no cache to wait out',
      async () => {
        const scopedClient = makeClient({ url: cfg.url, apiKey: scopedApiKey! })
        const res = await getProject({ client: scopedClient, path: { id: project.id } })
        if (!res.error || res.response?.status !== 401) {
          throw new Error(
            `GET /projects/${project.id} with a just-revoked key returned HTTP ${res.response?.status}, expected 401`,
          )
        }
      },
    )
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(
        `\n(kept resources: projects=${projectIds.join(',')} secondUser=${secondUserId} ` +
          `apiKey=${scopedApiKeyId})`,
      )
    } else {
      if (scopedApiKeyId !== undefined && !scopedApiKeyRevoked && sessionCookie) {
        // Best-effort: the deactivate step itself may be what failed.
        await fetch(`${apiBase}/api-keys/${scopedApiKeyId}/deactivate`, {
          method: 'POST',
          headers: { Cookie: sessionCookie },
        }).catch(() => undefined)
      }
      if (scopedApiKeyId !== undefined && sessionCookie) {
        await fetch(`${apiBase}/api-keys/${scopedApiKeyId}`, {
          method: 'DELETE',
          headers: { Cookie: sessionCookie },
        }).catch(() => undefined)
      }
      if (secondUserId !== undefined) {
        // `deleteUser` needs the primary identity's own permissions, not the
        // throwaway second user's -- same as rbac-scenario's teardown.
        await deleteUser({ client, path: { user_id: secondUserId } }).catch(() => undefined)
      }
      const td = await teardown(client, { projectIds })
      log(
        `\n▶ teardown: deleted ${td.deletedProjects} project(s), removed second user + api key` +
          (td.errors.length ? ` (${td.errors.length} errors)` : ''),
      )
      for (const e of td.errors) log(`    ! ${e}`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: ApiKeyScenarioResult = { runId, ok, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ api-key-scenario PASSED' : '❌ api-key-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
