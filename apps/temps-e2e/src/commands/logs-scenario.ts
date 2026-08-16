/**
 * Log-aggregation lifecycle against a live Temps instance -- proves REAL
 * container stdout/stderr flows through the actual Docker log collector
 * (temps-log-aggregator's CollectorService -> ChunkWriterService ->
 * LogSearchService) and comes back out through search correctly parsed,
 * not just that the search route returns 2xx for an empty result:
 *
 *   1. deploy a throwaway Go app (`log-emitter-app.ts`) that prints a
 *      structured JSON log line to stdout or stderr on demand over HTTP --
 *      the only way to get REAL lines through the REAL Docker log pipeline
 *      instead of inserting synthetic rows into storage directly
 *   2. emit an info-level line and an error-level line (with an extra
 *      `code` field), each tagged with a run-unique marker
 *   3. poll full-text search for the marker -- chunks flush on a 30s/1MB
 *      timer (see chunk_writer.rs), so a fresh line is NOT immediately
 *      queryable; this genuinely waits out that window instead of assuming
 *      log writes are synchronous
 *   4. assert both lines come back with the right computed `level` (parsed
 *      from the JSON `level` key, not stream-based) and the extra `code`
 *      field survived into the searchable `fields` JSONB
 *   5. assert a `levels: ["ERROR"]` filter narrows to just the error line
 *   6. assert an `envs` filter on the production environment's id also
 *      narrows correctly (proves env labels, not just text search, work)
 *   7. fetch grep-style context around the error line via chunk_id +
 *      line_offset and confirm the message is present in the raw window
 *   8. purge everything before "now"; confirm the marker is gone from
 *      search afterward -- the one destructive route this feature has
 *   9. teardown: delete the project (cascades the deployment)
 *
 * NOT covered: GET /logs/tail (SSE live streaming). It's a materially
 * different code path (broadcast-channel-fed, real-time) from the
 * chunk/search path this scenario exercises, but TailLogsData requires the
 * exact Docker-label `service`/`env` strings the collector stamped on the
 * container (not just project_id), which aren't exposed by any deploy
 * response -- verifying them would mean guessing at label conventions
 * instead of testing the documented contract. Left as a known gap rather
 * than a silent one.
 */
import { searchLogs, getLogContext, purgeProjectLogs } from '@temps-sdk/api'
import { makeClient, resolveConfig, unwrap } from '../lib/client.ts'
import {
  createE2eProject,
  getProductionEnvironment,
  deployImage,
  waitForDeployment,
  getDeployStatus,
  waitForHttpReady,
  assertNotConsoleFallback,
  resolveLoadTarget,
  teardown,
  makeRunId,
  pollUntil,
} from '../lib/flows.ts'
import { buildLogEmitterImage, emitLog } from '../lib/log-emitter-app.ts'

export interface LogsScenarioOptions {
  registry?: string
  keep?: boolean
  deployTimeout?: string
  json?: boolean
  connection: { url?: string; apiKey?: string }
}

interface StepLog {
  step: string
  ok: boolean
  detail?: string
  ms?: number
}

interface LogsScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
}

export async function logsScenarioCommand(opts: LogsScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps logs scenario  ->  ${cfg.url}`)

  const registry = opts.registry ?? process.env.TEMPS_E2E_REGISTRY
  if (!registry) {
    throw new Error(
      'A registry is required: the log-emitter image must be pushed somewhere the server can pull ' +
        'from. Pass --registry <host:port> (e.g. localhost:5111) or set TEMPS_E2E_REGISTRY. ' +
        'Start a local one with: docker run -d -p 5111:5000 --name temps-e2e-registry registry:2',
    )
  }

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

  const deployTimeoutMs = Number(opts.deployTimeout ?? '300000')
  const scratch = `${process.env.TMPDIR ?? '/tmp'}/temps-e2e-log-emitter`
  // chunks flush every 30s (or 1MB, whichever first) -- give a full extra
  // cycle of margin for search visibility.
  const CHUNK_FLUSH_TIMEOUT_MS = 90_000

  const projectIds: number[] = []
  const deployments: { projectId: number; deploymentId: number }[] = []

  try {
    const imageRef = await step('build + push log-emitter image', () =>
      buildLogEmitterImage({ scratchRoot: scratch, registry, onLog: (l) => log(`    ${l}`) }),
    )
    log(`  image: ${imageRef}`)

    const project = await step('create project', () =>
      createE2eProject(client, { name: `${runId}-logs-app`, exposedPort: 3000 }),
    )
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    const env = await step('resolve production environment', () => getProductionEnvironment(client, project.id))
    log(`  env #${env.id} (${env.name})`)

    const deploymentId = await step('deploy the log-emitter app', () =>
      deployImage(client, { projectId: project.id, environmentId: env.id, imageRef }),
    )
    deployments.push({ projectId: project.id, deploymentId })
    log(`  deployment #${deploymentId}`)

    await step('wait for deployment', () =>
      waitForDeployment(client, {
        projectId: project.id,
        deploymentId,
        timeoutMs: deployTimeoutMs,
        onPoll: (s) => log(`    ...${s.state}`),
      }),
    )

    await step('confirm deployment did not fail', async () => {
      const s = await getDeployStatus(client, project.id, deploymentId)
      if (!s.ok) throw new Error(`deployment ${deploymentId} is in state "${s.state}"`)
    })

    const target = resolveLoadTarget(cfg.url, env.mainUrl)
    await step('wait for the app to serve real traffic (not the console fallback)', async () => {
      await waitForHttpReady({ url: target.url, headers: target.headers })
      await assertNotConsoleFallback({ url: target.url, headers: target.headers })
    })

    const infoMsg = `${runId} unique-info-marker`
    const errorMsg = `${runId} unique-error-marker`

    await step('emit an info-level and an error-level (with a `code` field) log line', async () => {
      await emitLog(target, { level: 'info', msg: infoMsg })
      await emitLog(target, { level: 'error', msg: errorMsg, code: 502 })
    })

    const found = await step(
      'full-text search for the marker eventually finds both lines (chunk flush is up to 30s, not instant)',
      () =>
        pollUntil(
          () =>
            searchLogs({ client, body: { project_id: project.id, text: runId, page_size: 50 } }).then((r) =>
              unwrap(r, 'searchLogs'),
            ),
          (res) => res.lines.length >= 2,
          {
            timeoutMs: CHUNK_FLUSH_TIMEOUT_MS,
            intervalMs: 5000,
            label: 'log search visibility',
            onPoll: (res) => log(`    ...${res.lines.length} line(s) so far`),
          },
        ),
    )

    const infoLine = found.lines.find((l) => l.message === infoMsg)
    const errorLine = found.lines.find((l) => l.message === errorMsg)

    await step('the info line has level INFO and the error line has level ERROR (parsed from JSON, not stream)', async () => {
      if (!infoLine) throw new Error(`info marker not found in: ${JSON.stringify(found.lines)}`)
      if (!errorLine) throw new Error(`error marker not found in: ${JSON.stringify(found.lines)}`)
      if (infoLine.level !== 'INFO') throw new Error(`info line level="${infoLine.level}", expected "INFO"`)
      if (errorLine.level !== 'ERROR') throw new Error(`error line level="${errorLine.level}", expected "ERROR"`)
    })

    await step('the error line\'s extra `code` field survived into the searchable `fields` JSONB', async () => {
      const fields = errorLine!.fields as { code?: number } | undefined
      if (fields?.code !== 502) throw new Error(`error line fields=${JSON.stringify(errorLine!.fields)}, expected code=502`)
    })

    await step('a levels=["ERROR"] filter narrows to just the error line', async () => {
      const res = unwrap(
        await searchLogs({ client, body: { project_id: project.id, text: runId, levels: ['ERROR'], page_size: 50 } }),
        'searchLogs',
      )
      if (res.lines.length !== 1) throw new Error(`expected exactly 1 line with levels=[ERROR], got ${res.lines.length}: ${JSON.stringify(res.lines)}`)
      if (res.lines[0]!.message !== errorMsg) throw new Error(`filtered line message="${res.lines[0]!.message}", expected the error marker`)
    })

    await step('an envs filter on the production environment also narrows correctly', async () => {
      const res = unwrap(
        await searchLogs({
          client,
          body: { project_id: project.id, text: runId, envs: [String(env.id)], page_size: 50 },
        }),
        'searchLogs',
      )
      if (res.lines.length < 2) throw new Error(`envs filter unexpectedly dropped lines: ${JSON.stringify(res.lines)}`)
    })

    await step('grep-style context around the error line is retrievable via chunk_id + line_offset', async () => {
      const ctx = unwrap(
        await getLogContext({
          client,
          query: { chunk_id: errorLine!.chunk_id, line_offset: errorLine!.line_offset, lines: 5 },
        }),
        'getLogContext',
      )
      const ctxStr = JSON.stringify(ctx)
      if (!ctxStr.includes(errorMsg) && !ctxStr.includes('unique-error-marker')) {
        throw new Error(`log context did not include the error line: ${ctxStr}`)
      }
    })

    await step('purge everything before now; the marker is gone from search afterward', async () => {
      unwrap(
        await purgeProjectLogs({
          client,
          path: { project_id: project.id },
          body: { before: new Date(Date.now() + 1000).toISOString() },
        }),
        'purgeProjectLogs',
      )
      const res = unwrap(
        await searchLogs({ client, body: { project_id: project.id, text: runId, page_size: 50 } }),
        'searchLogs',
      )
      if (res.lines.length !== 0) throw new Error(`expected 0 lines after purge, got ${res.lines.length}: ${JSON.stringify(res.lines)}`)
    })
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(`\n(kept resources: projects=${projectIds.join(',')})`)
    } else {
      const td = await teardown(client, { deployments, projectIds })
      log(
        `\n▶ teardown: tore down ${td.teardownDeployments} deployment(s), ` +
          `deleted ${td.deletedProjects} project(s)` +
          (td.errors.length ? ` (${td.errors.length} errors)` : ''),
      )
      for (const e of td.errors) log(`    ! ${e}`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: LogsScenarioResult = { runId, ok, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ logs-scenario PASSED' : '❌ logs-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
