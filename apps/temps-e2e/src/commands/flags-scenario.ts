// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Feature flags lifecycle against a live Temps instance, driven through the
 * real `@temps-sdk/node-sdk` `FlagsClient` (not just the raw HTTP API) for
 * the read-path steps, since that's the actual client apps deployed on
 * Temps use:
 *   1. create a project + resolve its production environment
 *   2. create a bool flag and a string flag via the admin API
 *   3. mint a deployment token scoped to (project, environment) -- the same
 *      TEMPS_API_URL/TEMPS_API_TOKEN pair Temps injects into every real
 *      deployment
 *   4. `FlagsClient.refresh()` (real HTTP call, real ETag caching) then
 *      `get()`/`getDetails()`: both flags resolve to their declared
 *      defaults, and an unrecognized key resolves to the caller's fallback
 *   5. set a per-environment override via the admin API, `refresh()` again,
 *      confirm the override wins over the default
 *   6. set the kill switch (`enabled: false`) on a flag that ALSO carries an
 *      override value, `refresh()` again, confirm the flag reverts to its
 *      *default* -- proving the kill switch outranks any override, not just
 *      that it exists
 *   7. ETag/If-None-Match: a second `GET /flags/snapshot` with the ETag from
 *      the first response is a genuine 304
 *   8. exposure reporting: `getDetails()` on a flag marks it evaluated,
 *      `flushExposure()` reports it, and the admin API's `last_evaluated_at`
 *      (null beforehand) is now set -- proving the whole loop, not just that
 *      the endpoint accepts a POST
 *   9. auth boundary: the delivery endpoint rejects a plain admin API key
 *      with 400 (deployment-token-only, by design)
 *  10. teardown (delete the deployment token, then the project)
 *
 * Scope note: only boolean-style values, the kill switch, and per-environment
 * overrides are live in the shipped Phase 1 (docs/adr/034-feature-flags.md).
 * Percentage rollout and targeting rules exist in the schema but are dead
 * code server-side (`bucket()` is never called, `rules` is always `[]`), so
 * this scenario doesn't exercise them.
 */
import { FlagsClient } from '@temps-sdk/node-sdk'
import {
  createFlag,
  setFlagEnvironment,
  getFlag,
  getFlagSnapshot,
  createDeploymentToken,
  deleteDeploymentToken,
} from '@temps-sdk/api'
import { makeClient, resolveConfig, unwrap, normalizeApiUrl } from '../lib/client.ts'
import { createE2eProject, getProductionEnvironment, teardown, makeRunId } from '../lib/flows.ts'

export interface FlagsScenarioOptions {
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

interface FlagsScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
}

export async function flagsScenarioCommand(opts: FlagsScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps feature-flags scenario  ->  ${cfg.url}`)

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

  const boolKey = `${runId}.checkout`
  const stringKey = `${runId}.banner`
  const projectIds: number[] = []
  let tokenId: number | undefined
  let projectId: number | undefined

  try {
    const project = await step('create project', () => createE2eProject(client, { name: `${runId}-flags` }))
    projectId = project.id
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    const env = await step('resolve production environment', () => getProductionEnvironment(client, project.id))
    log(`  env #${env.id} (${env.name})`)

    await step('create a bool flag + a string flag', async () => {
      unwrap(
        await createFlag({
          client,
          path: { project_id: project.id },
          body: { key: boolKey, value_type: 'bool', default_value: false, client_visible: false },
        }),
        'createFlag(bool)',
      )
      unwrap(
        await createFlag({
          client,
          path: { project_id: project.id },
          body: { key: stringKey, value_type: 'string', default_value: 'default banner', client_visible: false },
        }),
        'createFlag(string)',
      )
    })

    const token = await step('mint a deployment token scoped to (project, environment)', async () => {
      const res = unwrap(
        await createDeploymentToken({
          client,
          path: { project_id: project.id },
          body: { name: `${runId}-token`, environment_id: env.id, deployment_id: null, permissions: null, expires_at: null },
        }),
        'createDeploymentToken',
      )
      return res
    })
    tokenId = token.id
    log(`  deployment token #${token.id}`)

    const flagsClient = new FlagsClient({
      apiUrl: normalizeApiUrl(cfg.url),
      apiToken: token.token,
      refreshIntervalMs: 0,
    })

    await step('FlagsClient.refresh(): defaults resolve, unknown key falls back', async () => {
      await flagsClient.refresh()
      if (!flagsClient.ready) {
        throw new Error(`FlagsClient not ready after refresh(): lastError=${flagsClient.lastError?.message}`)
      }
      const boolVal = flagsClient.get<boolean>(boolKey, {}, true)
      if (boolVal !== false) throw new Error(`${boolKey} resolved to ${boolVal}, expected its default (false)`)
      const stringVal = flagsClient.get<string>(stringKey, {}, 'MISSING')
      if (stringVal !== 'default banner') {
        throw new Error(`${stringKey} resolved to "${stringVal}", expected its default "default banner"`)
      }
      const unknown = flagsClient.get<string>(`${runId}.does-not-exist`, {}, 'my-fallback')
      if (unknown !== 'my-fallback') {
        throw new Error(`unknown key resolved to "${unknown}", expected the caller's fallback "my-fallback"`)
      }
    })

    await step('per-environment override wins over the default', async () => {
      unwrap(
        await setFlagEnvironment({
          client,
          path: { project_id: project.id, key: boolKey, environment_id: env.id },
          body: { value: true, enabled: true },
        }),
        'setFlagEnvironment',
      )
      await flagsClient.refresh()
      const boolVal = flagsClient.get<boolean>(boolKey, {}, false)
      if (boolVal !== true) throw new Error(`${boolKey} resolved to ${boolVal} after override, expected true`)
    })

    await step('kill switch outranks an override, not just the default', async () => {
      unwrap(
        await setFlagEnvironment({
          client,
          path: { project_id: project.id, key: stringKey, environment_id: env.id },
          body: { value: 'overridden banner', enabled: false },
        }),
        'setFlagEnvironment(enabled:false)',
      )
      await flagsClient.refresh()
      const details = flagsClient.getDetails<string>(stringKey, {}, 'MISSING')
      if (details.value !== 'default banner') {
        throw new Error(
          `${stringKey} resolved to "${details.value}" with the kill switch engaged, expected its default "default banner" -- the override must NOT win when enabled=false`,
        )
      }
    })

    await step('ETag/If-None-Match: a second identical fetch is a real 304', async () => {
      const deploymentClient = makeClient({ url: cfg.url, apiKey: token.token })
      const first = await getFlagSnapshot({ client: deploymentClient })
      if (first.response?.status !== 200) {
        throw new Error(`first getFlagSnapshot returned HTTP ${first.response?.status}, expected 200`)
      }
      const etag = first.response.headers.get('etag')
      if (!etag) throw new Error('first getFlagSnapshot response carried no ETag header')
      const second = await getFlagSnapshot({
        client: deploymentClient,
        headers: { 'If-None-Match': etag },
      })
      if (second.response?.status !== 304) {
        throw new Error(`second getFlagSnapshot (If-None-Match) returned HTTP ${second.response?.status}, expected 304`)
      }
    })

    await step('exposure reporting: evaluated -> flushed -> last_evaluated_at is set', async () => {
      const before = unwrap(
        await getFlag({ client, path: { project_id: project.id, key: boolKey } }),
        'getFlag(before)',
      )
      if (before.last_evaluated_at !== null) {
        throw new Error(`${boolKey}.last_evaluated_at was already set before any evaluation: ${before.last_evaluated_at}`)
      }
      flagsClient.getDetails<boolean>(boolKey, {}, false) // marks it evaluated in-memory
      await flagsClient.flushExposure()
      const after = unwrap(
        await getFlag({ client, path: { project_id: project.id, key: boolKey } }),
        'getFlag(after)',
      )
      if (!after.last_evaluated_at) {
        throw new Error(`${boolKey}.last_evaluated_at is still null after flushExposure()`)
      }
    })

    await step('auth boundary: a plain admin API key is rejected on the delivery endpoint (400)', async () => {
      const res = await getFlagSnapshot({ client })
      if (!res.error) {
        throw new Error(`getFlagSnapshot with an admin API key succeeded, expected HTTP 400`)
      }
      const status = res.response?.status
      if (status !== 400) {
        throw new Error(`getFlagSnapshot with an admin API key returned HTTP ${status}, expected 400`)
      }
    })
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(`\n(kept resources: projects=${projectIds.join(',')})`)
    } else {
      if (tokenId && projectId) {
        await deleteDeploymentToken({ client, path: { project_id: projectId, token_id: tokenId } }).catch(() => undefined)
      }
      const td = await teardown(client, { projectIds })
      log(
        `\n▶ teardown: deleted deployment token + ${td.deletedProjects} project(s)` +
          (td.errors.length ? ` (${td.errors.length} errors)` : ''),
      )
      for (const e of td.errors) log(`    ! ${e}`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: FlagsScenarioResult = { runId, ok, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ flags-scenario PASSED' : '❌ flags-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
