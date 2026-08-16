/**
 * Error-tracking (Sentry-compatible) lifecycle against a live Temps instance --
 * proves real error events get authenticated, ingested, fingerprint-grouped,
 * and queryable, not just that the CRUD routes around a group return 2xx:
 *
 *   1. create a project + a DSN for it (POST /projects/{id}/dsns) -- DSNs are
 *      NOT auto-created with a project, unlike the monitoring auto-monitor;
 *      a real SDK integration always starts with this step
 *   2. confirm a fresh project has zero error groups (error-stats + list)
 *   3. send a real Sentry-shaped event (exception type/value/stacktrace) via
 *      raw POST to /{project_id}/store/ authenticated with the DSN's public
 *      key (X-Sentry-Auth header) -- NOT the bearer token; this is the one
 *      route in the platform that intentionally uses a different auth
 *      scheme, so this is the only coverage that exercises it for real
 *   4. assert it created exactly one error group with the expected computed
 *      title ("{type}: {value}"), error_type, status "unresolved", and
 *      total_count 1; fetch its single event back and confirm the exception
 *      data round-tripped
 *   5. send a SECOND event with the identical exception type/value/stack --
 *      proves fingerprint-based grouping actually groups repeats instead of
 *      creating a new issue per event (total_count -> 2, still 1 group)
 *   6. send a THIRD event with a completely different exception/stack --
 *      proves it does NOT get merged into the first group (2 groups now)
 *   7. resolve the first group (PUT status=resolved) and confirm error-stats
 *      reflects the split (1 unresolved, 1 resolved)
 *   8. confirm a request with a garbage DSN key is rejected 401 -- the only
 *      real auth-boundary check this endpoint has
 *   9. teardown: delete the project (cascades the DSN + both error groups)
 */
import { createDsn, getErrorGroup, getErrorEvent, listErrorEvents, listErrorGroups, getErrorStats, updateErrorGroup } from '@temps-sdk/api'
import { makeClient, normalizeApiUrl, resolveConfig, unwrap } from '../lib/client.ts'
import { createE2eProject, getProductionEnvironment, teardown, makeRunId } from '../lib/flows.ts'
import { buildSentryEvent, sendSentryEvent } from '../lib/sentry-events.ts'

export interface ErrorTrackingScenarioOptions {
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

interface ErrorTrackingScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
}

export async function errorTrackingScenarioCommand(opts: ErrorTrackingScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const apiBaseUrl = normalizeApiUrl(cfg.url)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps error-tracking scenario  ->  ${cfg.url}`)

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
  let groupId: number | undefined
  let secondGroupId: number | undefined

  try {
    const project = await step('create project', () =>
      createE2eProject(client, { name: `${runId}-error-tracking` }),
    )
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    const env = await step('resolve production environment', () => getProductionEnvironment(client, project.id))
    log(`  env #${env.id} (${env.name})`)

    const dsn = await step('create a DSN for the project (not auto-created, unlike the monitoring auto-monitor)', () =>
      createDsn({
        client,
        path: { project_id: project.id },
        body: { name: `${runId}-dsn`, environment_id: env.id },
      }).then((r) => unwrap(r, 'createDsn')),
    )
    log(`  dsn #${dsn.id} public_key=${dsn.public_key}`)

    await step('a fresh project has zero error groups', async () => {
      const stats = unwrap(
        await getErrorStats({ client, path: { project_id: project.id } }),
        'getErrorStats',
      )
      if (stats.total_groups !== 0) throw new Error(`total_groups=${stats.total_groups}, expected 0`)
      const groups = unwrap(
        await listErrorGroups({ client, path: { project_id: project.id } }),
        'listErrorGroups',
      )
      if (groups.data.length !== 0) throw new Error(`error groups list not empty: ${JSON.stringify(groups.data)}`)
    })

    const exceptionValue = `first synthetic failure ${runId}`
    const firstEvent = buildSentryEvent({
      exceptionType: 'SyntheticTestError',
      exceptionValue,
      frames: [
        { filename: 'app.js', function: 'handleRequest', lineno: 10, colno: 5 },
        { filename: 'server.js', function: 'listen', lineno: 20, colno: 3 },
        { filename: 'index.js', function: 'main', lineno: 1, colno: 1 },
      ],
    })

    await step('send a real Sentry event via DSN auth (not the bearer token) to /{project_id}/store/', async () => {
      const res = await sendSentryEvent({ apiBaseUrl, projectId: project.id, dsnPublicKey: dsn.public_key, event: firstEvent })
      if (res.status !== 200) throw new Error(`store returned HTTP ${res.status}: ${JSON.stringify(res.body)}`)
    })

    await step('it created exactly one error group with the expected title/type/status', async () => {
      const groups = unwrap(
        await listErrorGroups({ client, path: { project_id: project.id } }),
        'listErrorGroups',
      )
      if (groups.data.length !== 1) throw new Error(`expected 1 error group, got ${groups.data.length}`)
      const group = groups.data[0]!
      groupId = group.id
      const expectedTitle = `SyntheticTestError: ${exceptionValue}`
      if (group.title !== expectedTitle) throw new Error(`title="${group.title}", expected "${expectedTitle}"`)
      if (group.error_type !== 'SyntheticTestError') throw new Error(`error_type="${group.error_type}", expected "SyntheticTestError"`)
      if (group.status !== 'unresolved') throw new Error(`status="${group.status}", expected "unresolved"`)
      if (group.total_count !== 1) throw new Error(`total_count=${group.total_count}, expected 1`)
    })
    log(`  group #${groupId}`)

    await step('the group has exactly one event, and its detail round-trips the exception data', async () => {
      const events = unwrap(
        await listErrorEvents({ client, path: { project_id: project.id, group_id: groupId! } }),
        'listErrorEvents',
      )
      if (events.data.length !== 1) throw new Error(`expected 1 event in group, got ${events.data.length}`)
      const eventDetail = unwrap(
        await getErrorEvent({ client, path: { project_id: project.id, group_id: groupId!, event_id: events.data[0]!.id } }),
        'getErrorEvent',
      )
      // The stored event wraps the raw Sentry payload under a `sentry` key
      // alongside a `source` discriminator (see error_events.data's shape --
      // this crate stores events from multiple SDK sources, not just Sentry).
      const data = eventDetail.data as
        | { sentry?: { exception?: { values?: Array<{ type?: string; value?: string }> } } }
        | undefined
      const exc = data?.sentry?.exception?.values?.[0]
      if (exc?.type !== 'SyntheticTestError' || exc?.value !== exceptionValue) {
        throw new Error(`event data did not round-trip the exception: ${JSON.stringify(data)}`)
      }
    })

    await step('a second, IDENTICAL exception groups into the SAME issue (fingerprint match)', async () => {
      const res = await sendSentryEvent({
        apiBaseUrl,
        projectId: project.id,
        dsnPublicKey: dsn.public_key,
        event: buildSentryEvent({
          exceptionType: 'SyntheticTestError',
          exceptionValue,
          frames: [
            { filename: 'app.js', function: 'handleRequest', lineno: 10, colno: 5 },
            { filename: 'server.js', function: 'listen', lineno: 20, colno: 3 },
            { filename: 'index.js', function: 'main', lineno: 1, colno: 1 },
          ],
        }),
      })
      if (res.status !== 200) throw new Error(`store returned HTTP ${res.status}: ${JSON.stringify(res.body)}`)

      const groups = unwrap(
        await listErrorGroups({ client, path: { project_id: project.id } }),
        'listErrorGroups',
      )
      if (groups.data.length !== 1) throw new Error(`expected still 1 error group after a repeat, got ${groups.data.length}`)
      const group = unwrap(
        await getErrorGroup({ client, path: { project_id: project.id, group_id: groupId! } }),
        'getErrorGroup',
      )
      if (group.total_count !== 2) throw new Error(`total_count=${group.total_count}, expected 2 after the repeat`)
    })

    await step('a third, DIFFERENT exception creates a SEPARATE issue (not merged)', async () => {
      const res = await sendSentryEvent({
        apiBaseUrl,
        projectId: project.id,
        dsnPublicKey: dsn.public_key,
        event: buildSentryEvent({
          exceptionType: 'SyntheticDifferentError',
          exceptionValue: `a completely unrelated failure mode ${runId}`,
          frames: [
            { filename: 'totally-different-file.js', function: 'unrelatedHandler', lineno: 99, colno: 9 },
            { filename: 'other.js', function: 'otherFn', lineno: 55, colno: 2 },
          ],
        }),
      })
      if (res.status !== 200) throw new Error(`store returned HTTP ${res.status}: ${JSON.stringify(res.body)}`)

      const groups = unwrap(
        await listErrorGroups({ client, path: { project_id: project.id } }),
        'listErrorGroups',
      )
      if (groups.data.length !== 2) throw new Error(`expected 2 error groups after a distinct exception, got ${groups.data.length}`)
      secondGroupId = groups.data.find((g) => g.id !== groupId)?.id
      if (!secondGroupId) throw new Error(`could not find the new group among ${JSON.stringify(groups.data)}`)
    })
    log(`  second group #${secondGroupId}`)

    await step('error-stats reflects 2 groups, both unresolved', async () => {
      const stats = unwrap(
        await getErrorStats({ client, path: { project_id: project.id } }),
        'getErrorStats',
      )
      if (stats.total_groups !== 2) throw new Error(`total_groups=${stats.total_groups}, expected 2`)
      if (stats.unresolved_groups !== 2) throw new Error(`unresolved_groups=${stats.unresolved_groups}, expected 2`)
      if (stats.resolved_groups !== 0) throw new Error(`resolved_groups=${stats.resolved_groups}, expected 0`)
    })

    await step('resolve the first group; error-stats splits 1 resolved / 1 unresolved', async () => {
      // UpdateErrorGroupResponses' 200 body is untyped (`unknown`) in the
      // generated SDK, so verify via a fresh typed GET instead of the
      // mutation response.
      unwrap(
        await updateErrorGroup({
          client,
          path: { project_id: project.id, group_id: groupId! },
          body: { status: 'resolved' },
        }),
        'updateErrorGroup',
      )
      const reread = unwrap(
        await getErrorGroup({ client, path: { project_id: project.id, group_id: groupId! } }),
        'getErrorGroup',
      )
      if (reread.status !== 'resolved') throw new Error(`status="${reread.status}", expected "resolved"`)

      const stats = unwrap(
        await getErrorStats({ client, path: { project_id: project.id } }),
        'getErrorStats',
      )
      if (stats.resolved_groups !== 1) throw new Error(`resolved_groups=${stats.resolved_groups}, expected 1`)
      if (stats.unresolved_groups !== 1) throw new Error(`unresolved_groups=${stats.unresolved_groups}, expected 1`)
    })

    await step('a garbage DSN key is rejected 401 (the one auth boundary specific to this route)', async () => {
      const res = await sendSentryEvent({
        apiBaseUrl,
        projectId: project.id,
        dsnPublicKey: 'not-a-real-dsn-key',
        event: buildSentryEvent({
          exceptionType: 'ShouldNeverBeStored',
          exceptionValue: 'this must be rejected before ingestion',
          frames: [{ filename: 'x.js', function: 'x', lineno: 1, colno: 1 }],
        }),
      })
      if (res.status !== 401) throw new Error(`store with a garbage DSN key returned HTTP ${res.status}, expected 401`)
    })
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(`\n(kept resources: projects=${projectIds.join(',')} group=${groupId} secondGroup=${secondGroupId})`)
    } else {
      const td = await teardown(client, { projectIds })
      log(
        `\n▶ teardown: deleted ${td.deletedProjects} project(s) (cascades the DSN + both error groups)` +
          (td.errors.length ? ` (${td.errors.length} errors)` : ''),
      )
      for (const e of td.errors) log(`    ! ${e}`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: ErrorTrackingScenarioResult = { runId, ok, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ error-tracking-scenario PASSED' : '❌ error-tracking-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
