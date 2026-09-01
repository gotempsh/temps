// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Audit-log lifecycle against a live Temps instance:
 *   1. capture a wall-clock lower bound, then create a real project (a
 *      synchronous PROJECT_CREATED audit write, no queue/aggregation delay)
 *   2. read it back via GET /audit/logs (list, filtered) AND GET
 *      /audit/logs/{id} (single), asserting an EXACT match on id/data/actor/
 *      timestamp -- not just "the list is non-empty"
 *
 * Real bug found + fixed while building this: `from`/`to` are documented
 * (Rust doc comment, OpenAPI `example`, and by extension this package's
 * generated SDK types) as "milliseconds since epoch", but
 * `temps_core::DateTime`'s Deserialize impl only ever accepts ISO 8601
 * strings -- passing millis 400s with "Invalid datetime format". Fixed the
 * doc/example in crates/temps-audit/src/handlers/types.rs; this file passes
 * real ISO 8601 strings, matching actual server behavior.
 *   3. delete the project (a real PROJECT_DELETED audit write) and read that
 *      back the same way
 *   4. confirm the read endpoint itself is RBAC-gated: the same request
 *      without a bearer token is rejected, not silently scoped to nothing
 *
 * The project delete happens mid-scenario (it's the trigger for the
 * PROJECT_DELETED assertion), but a `finally` still cleans it up if an
 * earlier step throws first -- audit_logs rows themselves are intentionally
 * permanent and are never cleaned up.
 */
import { listAuditLogs, getAuditLog, deleteProject, getCurrentUser } from '@temps-sdk/api'
import { makeClient, resolveConfig, unwrap, normalizeApiUrl } from '../lib/client.ts'
import { createE2eProject, teardown, makeRunId } from '../lib/flows.ts'

export interface AuditScenarioOptions {
  json?: boolean
  connection: { url?: string; apiKey?: string }
}

interface StepLog {
  step: string
  ok: boolean
  detail?: string
  ms?: number
}

interface AuditScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
}

/** Find, among possibly-many rows for this operation_type/user_id, the one whose `data` matches this run's project. */
function findRowForProject<T extends { data?: unknown }>(rows: T[], projectId: number): T | undefined {
  return rows.find((r) => {
    const d = r.data as { project_id?: number } | undefined
    return d?.project_id === projectId
  })
}

export async function auditScenarioCommand(opts: AuditScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps audit-logs scenario  ->  ${cfg.url}`)

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

  let projectId: number | undefined
  let projectDeletedByStep = false

  try {
    const me = await step('resolve the authenticated actor (GET /user/me)', async () => {
      const u = unwrap(await getCurrentUser({ client }), 'getCurrentUser')
      return u
    })
    log(`  actor: user #${me.id} (${me.email})`)

    const beforeCreateMs = Date.now()
    const project = await step('create project (real, synchronous PROJECT_CREATED audit write)', () =>
      createE2eProject(client, { name: `${runId}-audit` }),
    )
    projectId = project.id
    log(`  project #${project.id} (${project.slug})`)

    const createdRow = await step('read back PROJECT_CREATED via list (exact row match, not vanity non-empty)', async () => {
      const rows = unwrap(
        await listAuditLogs({
          client,
          query: {
            operation_type: 'PROJECT_CREATED',
            user_id: me.id,
            from: new Date(beforeCreateMs - 1000).toISOString(),
            to: new Date(Date.now() + 1000).toISOString(),
            limit: 50,
            offset: 0,
          },
        }),
        'listAuditLogs',
      ) as Array<{ id: number; audit_date: number; data?: unknown; user_id: number; operation_type: string }>
      const row = findRowForProject(rows, project.id)
      if (!row) {
        throw new Error(
          `no PROJECT_CREATED audit row found for project ${project.id} among ${rows.length} candidate(s)`,
        )
      }
      const data = row.data as { project_id?: number; project_slug?: string; project_name?: string } | undefined
      if (data?.project_id !== project.id) {
        throw new Error(`audit row data.project_id=${data?.project_id}, expected ${project.id}`)
      }
      if (data?.project_slug !== project.slug) {
        throw new Error(`audit row data.project_slug="${data?.project_slug}", expected "${project.slug}"`)
      }
      if (row.audit_date < beforeCreateMs - 1000 || row.audit_date > Date.now() + 1000) {
        throw new Error(
          `audit row audit_date=${row.audit_date} is outside the expected window [${beforeCreateMs}, ${Date.now()}] -- likely a stale row matched by coincidence`,
        )
      }
      if (row.user_id !== me.id) {
        throw new Error(`audit row user_id=${row.user_id}, expected the actual actor ${me.id}`)
      }
      return row
    })

    await step('read the same row individually via GET /audit/logs/{id}', async () => {
      const single = unwrap(await getAuditLog({ client, path: { id: createdRow.id } }), 'getAuditLog')
      if (single.id !== createdRow.id) {
        throw new Error(`getAuditLog(${createdRow.id}) returned id=${single.id}`)
      }
      const data = single.data as { project_id?: number } | undefined
      if (data?.project_id !== project.id) {
        throw new Error(`getAuditLog(${createdRow.id}).data.project_id=${data?.project_id}, expected ${project.id}`)
      }
    })

    const beforeDeleteMs = Date.now()
    await step('delete project (real, synchronous PROJECT_DELETED audit write)', async () => {
      await deleteProject({ client, path: { id: project.id } })
    })
    projectDeletedByStep = true

    await step('read back PROJECT_DELETED via list (exact row match)', async () => {
      const rows = unwrap(
        await listAuditLogs({
          client,
          query: {
            operation_type: 'PROJECT_DELETED',
            user_id: me.id,
            from: new Date(beforeDeleteMs - 1000).toISOString(),
            to: new Date(Date.now() + 1000).toISOString(),
            limit: 50,
            offset: 0,
          },
        }),
        'listAuditLogs',
      ) as Array<{ id: number; data?: unknown }>
      const row = findRowForProject(rows, project.id)
      if (!row) {
        throw new Error(
          `no PROJECT_DELETED audit row found for project ${project.id} among ${rows.length} candidate(s)`,
        )
      }
    })

    await step('confirm the read endpoint is actually gated (no bearer token -> rejected)', async () => {
      const res = await fetch(`${normalizeApiUrl(cfg.url)}/audit/logs?limit=1&offset=0`)
      if (res.status !== 401 && res.status !== 403) {
        throw new Error(`GET /audit/logs with no Authorization header returned HTTP ${res.status}, expected 401/403`)
      }
    })
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (projectId !== undefined && !projectDeletedByStep) {
      const td = await teardown(client, { projectIds: [projectId] })
      log(`\n▶ teardown: cleaned up project #${projectId} left over from an earlier failed step`)
      for (const e of td.errors) log(`    ! ${e}`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: AuditScenarioResult = { runId, ok, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ audit-scenario PASSED' : '❌ audit-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
