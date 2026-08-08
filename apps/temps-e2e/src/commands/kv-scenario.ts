/**
 * kv-storage lifecycle against a live Temps instance:
 *   1. ensure the platform-wide KV (Redis) service is enabled + healthy --
 *      never disabled in teardown, since it's a shared singleton other
 *      concurrent runs/users may depend on
 *   2. real set/get round trip (exact deep-equal, not just "not null")
 *   3. atomic incr semantics (sequential increments, fresh-key default)
 *   4. keys() pattern matching (exact set match, no leftover pollution)
 *   5. nx/xx conditional-write semantics
 *   6. expire/ttl sentinel values (-1 no expiration, -2 missing key)
 *   7. del (exact deleted-count, idempotent second call)
 *   8. cross-project tenant isolation (the `kv:p{project_id}:` namespace
 *      prefix actually isolates data, not just documented to)
 *   9. API-key auth requires project_id in the body (400, not a silent
 *      project 0/null)
 *  10. teardown (delete the two throwaway projects; leave KV enabled)
 *
 * The web console has no UI to browse KV values (only kv_status) -- this is
 * data-plane API coverage, there is no companion Playwright spec to write.
 */
import { kvGet, kvSet, kvIncr, kvDel, kvExpire, kvTtl, kvKeys, kvStatus } from '@temps-sdk/api'
import { makeClient, resolveConfig, unwrap } from '../lib/client.ts'
import { createE2eProject, ensureKvEnabled, teardown, makeRunId } from '../lib/flows.ts'

export interface KvScenarioOptions {
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

interface KvScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
}

export async function kvScenarioCommand(opts: KvScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps kv-storage scenario  ->  ${cfg.url}`)

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

  const projectIds: number[] = []
  const userKey = `${runId}:user`
  const counterKey = `${runId}:counter`
  const counterFreshKey = `${runId}:counter-fresh`
  const xxKey = `${runId}:xxtest`
  const sharedKey = `${runId}:shared`
  const originalUser = { name: 'e2e', n: 1 }

  try {
    await step('ensure kv-storage is enabled + healthy', () => ensureKvEnabled(client))

    const projectA = await step('create project A', () => createE2eProject(client, { name: `${runId}-kv-a` }))
    projectIds.push(projectA.id)
    log(`  project A #${projectA.id} (${projectA.slug})`)

    const projectB = await step('create project B (for isolation check)', () =>
      createE2eProject(client, { name: `${runId}-kv-b` }),
    )
    projectIds.push(projectB.id)
    log(`  project B #${projectB.id} (${projectB.slug})`)

    await step('set -> get round trip (exact value match)', async () => {
      const set = unwrap(
        await kvSet({ client, body: { key: userKey, value: originalUser, ex: 300, project_id: projectA.id } }),
        'kvSet',
      )
      if (set.result !== 'OK') throw new Error(`kvSet returned result="${set.result}", expected "OK"`)
      const got = unwrap(await kvGet({ client, body: { key: userKey, project_id: projectA.id } }), 'kvGet')
      if (JSON.stringify(got.value) !== JSON.stringify(originalUser)) {
        throw new Error(`kvGet returned ${JSON.stringify(got.value)}, expected ${JSON.stringify(originalUser)}`)
      }
    })

    await step('incr: sequential increments + fresh-key default', async () => {
      let last = 0
      for (let i = 0; i < 3; i++) {
        const r = unwrap(await kvIncr({ client, body: { key: counterKey, project_id: projectA.id } }), 'kvIncr')
        last = r.value
      }
      if (last !== 3) throw new Error(`after 3 sequential kvIncr calls, value=${last}, expected 3`)

      const fresh = unwrap(
        await kvIncr({ client, body: { key: counterFreshKey, project_id: projectA.id } }),
        'kvIncr',
      )
      if (fresh.value !== 1) {
        throw new Error(`kvIncr on a brand-new key returned value=${fresh.value}, expected 1`)
      }
    })

    await step('keys: pattern match returns exactly what this run wrote (no more, no fewer)', async () => {
      const res = unwrap(
        await kvKeys({ client, body: { pattern: `${runId}:*`, project_id: projectA.id } }),
        'kvKeys',
      )
      const expected = [userKey, counterKey, counterFreshKey].sort()
      const actual = [...res.keys].sort()
      if (JSON.stringify(actual) !== JSON.stringify(expected)) {
        throw new Error(`kvKeys returned ${JSON.stringify(actual)}, expected ${JSON.stringify(expected)}`)
      }
    })

    await step('nx: conditional write against an existing key leaves it unchanged', async () => {
      await kvSet({
        client,
        body: { key: userKey, value: { name: 'should-not-land' }, nx: true, project_id: projectA.id },
      })
      const got = unwrap(await kvGet({ client, body: { key: userKey, project_id: projectA.id } }), 'kvGet')
      if (JSON.stringify(got.value) !== JSON.stringify(originalUser)) {
        throw new Error(
          `nx:true write against an existing key overwrote it: got ${JSON.stringify(got.value)}, expected the untouched original ${JSON.stringify(originalUser)}`,
        )
      }
    })

    await step('xx: conditional write against a deleted key does not create it', async () => {
      unwrap(
        await kvSet({ client, body: { key: xxKey, value: 'v1', project_id: projectA.id } }),
        'kvSet',
      )
      unwrap(await kvDel({ client, body: { keys: [xxKey], project_id: projectA.id } }), 'kvDel')
      await kvSet({ client, body: { key: xxKey, value: 'v2', xx: true, project_id: projectA.id } })
      const got = unwrap(await kvGet({ client, body: { key: xxKey, project_id: projectA.id } }), 'kvGet')
      if (got.value !== undefined && got.value !== null) {
        throw new Error(`xx:true write against a non-existent key created it: got ${JSON.stringify(got.value)}`)
      }
    })

    await step('ttl sentinels: -1 for no expiration, -2 for a missing key', async () => {
      const noExpiry = unwrap(
        await kvTtl({ client, body: { key: counterFreshKey, project_id: projectA.id } }),
        'kvTtl',
      )
      if (noExpiry.ttl !== -1) {
        throw new Error(`kvTtl on a key set without ex/px returned ${noExpiry.ttl}, expected -1`)
      }
      const missing = unwrap(
        await kvTtl({ client, body: { key: `${runId}:never-created`, project_id: projectA.id } }),
        'kvTtl',
      )
      if (missing.ttl !== -2) {
        throw new Error(`kvTtl on a never-created key returned ${missing.ttl}, expected -2`)
      }
    })

    await step('expire: sets a real, bounded TTL', async () => {
      const exp = unwrap(
        await kvExpire({ client, body: { key: counterFreshKey, seconds: 120, project_id: projectA.id } }),
        'kvExpire',
      )
      if (!exp.success) throw new Error('kvExpire on an existing key returned success=false')
      const ttl = unwrap(
        await kvTtl({ client, body: { key: counterFreshKey, project_id: projectA.id } }),
        'kvTtl',
      )
      if (!(ttl.ttl > 0 && ttl.ttl <= 120)) {
        throw new Error(`kvTtl after kvExpire(120) returned ${ttl.ttl}, expected in (0, 120]`)
      }
    })

    await step('del: exact deleted-count, idempotent on a second call', async () => {
      const first = unwrap(
        await kvDel({ client, body: { keys: [userKey, counterKey], project_id: projectA.id } }),
        'kvDel',
      )
      if (first.deleted !== 2) throw new Error(`kvDel on 2 existing keys returned deleted=${first.deleted}, expected 2`)
      const second = unwrap(
        await kvDel({ client, body: { keys: [userKey, counterKey], project_id: projectA.id } }),
        'kvDel',
      )
      if (second.deleted !== 0) {
        throw new Error(`kvDel on already-deleted keys returned deleted=${second.deleted}, expected 0`)
      }
    })

    await step('cross-project isolation: same key, different project_id, different data', async () => {
      unwrap(
        await kvSet({ client, body: { key: sharedKey, value: 'project-A-secret', project_id: projectA.id } }),
        'kvSet',
      )
      const fromB = unwrap(await kvGet({ client, body: { key: sharedKey, project_id: projectB.id } }), 'kvGet')
      if (fromB.value !== undefined && fromB.value !== null) {
        throw new Error(
          `project B read project A's value for the same literal key: ${JSON.stringify(fromB.value)} -- the kv:p{project_id}: namespace is not isolating tenants`,
        )
      }
      const fromA = unwrap(await kvGet({ client, body: { key: sharedKey, project_id: projectA.id } }), 'kvGet')
      if (fromA.value !== 'project-A-secret') {
        throw new Error(`project A's own read returned ${JSON.stringify(fromA.value)}, expected "project-A-secret"`)
      }
    })

    await step('console read-back: kv_status reports enabled AND healthy', async () => {
      const status = unwrap(await kvStatus({ client }), 'kvStatus')
      if (!status.enabled || !status.healthy) {
        throw new Error(`kv_status returned enabled=${status.enabled} healthy=${status.healthy}, expected both true`)
      }
    })

    await step('API-key auth requires project_id in the body (400, not a silent no-op)', async () => {
      const res = await kvGet({ client, body: { key: `${runId}:no-project-id` } })
      if (!res.error) {
        throw new Error(
          `kvGet with no project_id succeeded (value=${JSON.stringify(res.data?.value)}), expected HTTP 400`,
        )
      }
      const status = res.response?.status
      if (status !== 400) {
        throw new Error(`kvGet with no project_id returned HTTP ${status}, expected 400`)
      }
    })
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(`\n(kept resources: projects=${projectIds.join(',')}; kv-storage left enabled)`)
    } else {
      // Best-effort key cleanup; project deletion below removes the projects
      // regardless, and stray Redis keys under a dead project id are harmless.
      await kvDel({ client, body: { keys: [xxKey, sharedKey], project_id: projectIds[0] } }).catch(() => undefined)
      const td = await teardown(client, { projectIds })
      log(
        `\n▶ teardown: deleted ${td.deletedProjects} project(s); kv-storage left enabled` +
          (td.errors.length ? ` (${td.errors.length} errors)` : ''),
      )
      for (const e of td.errors) log(`    ! ${e}`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: KvScenarioResult = { runId, ok, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ kv-scenario PASSED' : '❌ kv-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
