// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Uptime-monitoring (status-page) lifecycle against a live Temps instance --
 * proves the whole chain actually detects and reports a real outage, not
 * just that monitor/incident CRUD returns 2xx:
 *
 *   1. create a project -- confirm its production environment gets an
 *      AUTO-CREATED default monitor (EnvironmentCreated -> plugin.rs ->
 *      ensure_monitor_for_environment); most projects never get an explicit
 *      monitor, so this auto-provisioning path is the one real users
 *      actually depend on
 *   2. deploy a throwaway Go app (`toggle-app.ts`) whose health can be
 *      flipped over HTTP (POST /toggle?state=up|down) -- the only way to
 *      make a REAL deployed app start failing without touching Docker/the
 *      host, so it works identically against a remote instance
 *   3. create a second, explicit monitor via the API (covers the CRUD path
 *      the auto-provisioned one doesn't exercise) and drive it through the
 *      rest of the scenario
 *   4. confirm the MonitorCreated event triggers an IMMEDIATE first check
 *      (not a 60s wait): current-status goes from "unknown" to "operational"
 *      within a few seconds
 *   5. toggle the app down -- wait for the next periodic check cycle
 *      (health checks are a FIXED 60s global interval with no per-monitor
 *      on-demand trigger, so this genuinely waits ~60-90s) and assert:
 *        - current-status flips to "major_outage" (the raw status
 *          health_check_service.rs writes for 5xx-after-retries -- NOT the
 *          literal string "down")
 *        - a real incident is created (severity "major", status
 *          "investigating", monitor_id matches)
 *        - the bucketed chart's down_count/status reflect the outage
 *        - the project-level status overview flips to "partial_outage"
 *   6. toggle the app back up -- wait for the next cycle and assert the
 *      incident auto-resolves and everything returns to "operational"
 *   7. delete the explicit monitor; teardown the deployment + project
 *      (cascades the environment, and with it the auto-created monitor)
 *
 * Real bugs found + fixed while building this (see the two preceding commits
 * on this branch):
 *  - health_check_service.rs writes the finer-grained "major_outage"/
 *    "partial_outage" statuses, but calculate_overall_status and
 *    get_bucketed_status's SQL only recognized literal "down"/"degraded" --
 *    a fully-down monitor was invisible to both unless an incident also
 *    happened to exist to catch it via a separate code path. Step 5's
 *    bucketed-status assertion is a direct regression test for this.
 *  - CurrentStatusQuery/UptimeQuery/BucketedQuery declared start_time/
 *    end_time as non-optional, so every call had to pass an explicit range
 *    or 400 -- despite the docs (and, for uptime, the `days` field) all
 *    promising sensible defaults when omitted. Step 4's current-status poll
 *    calls with NO query params at all, the way any real client naturally
 *    would.
 *  - create_monitor's synthetic "unknown" bootstrap row counted toward every
 *    uptime denominator without ever counting as a success, so a monitor
 *    with a 100% real pass rate reported <100% uptime for as long as that
 *    placeholder row stayed in the window; plugin.rs also built TWO separate
 *    MonitorService instances (one job-queue-less for the HTTP handlers, one
 *    with a queue for the job listener), so a monitor created through the
 *    real API never got the "checked immediately" treatment the auto-
 *    provisioned path did. Step 4's ~2s convergence is a regression test.
 *  - get_status_overview's per-monitor status came from
 *    calculate_avg_response_time, whose SQL selected AVG(response_time_ms)
 *    (Postgres NUMERIC) into an `Option<f64>` FromQueryResult field --
 *    sqlx's try_get<f64> errored on the type mismatch, and the caller
 *    silently swallowed the Err into a fake "unknown" status for every
 *    monitor, even fully healthy ones. Steps 4b/6's status-overview
 *    assertions are a direct regression test.
 *  - incident auto-resolution is a step 6 red herring waiting to happen:
 *    current-status reads status_checks directly and flips the instant a
 *    health check commits, but incident resolution is a side effect of the
 *    SAME check processed asynchronously through the job queue
 *    (StatusCheckCompleted -> OutageDetectionService), which trails by
 *    anywhere from a few ms to, under heavy ambient load (many other
 *    monitors ticking on the same instance), most of another 60s cycle --
 *    asserting it once immediately after observing recovery is inherently
 *    racy, so that step polls too.
 */
import {
  listMonitors,
  createMonitor,
  getMonitor,
  deleteMonitor,
  getCurrentMonitorStatus,
  getUptimeHistory,
  getBucketedStatus,
  getStatusOverview,
  listIncidents,
  getIncident,
} from '@temps-sdk/api'
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
import { buildToggleImage, setToggleState } from '../lib/toggle-app.ts'

export interface MonitoringScenarioOptions {
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

interface MonitoringScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
}

export async function monitoringScenarioCommand(opts: MonitoringScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps monitoring scenario  ->  ${cfg.url}`)

  const registry = opts.registry ?? process.env.TEMPS_E2E_REGISTRY
  if (!registry) {
    throw new Error(
      'A registry is required: the toggle app image must be pushed somewhere the server can pull ' +
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
  const scratch = `${process.env.TMPDIR ?? '/tmp'}/temps-e2e-toggle`
  // health checks run on a FIXED 60s global interval with no per-monitor
  // on-demand trigger -- give a full extra cycle of margin.
  const CHECK_CYCLE_TIMEOUT_MS = 150_000

  const projectIds: number[] = []
  const deployments: { projectId: number; deploymentId: number }[] = []
  let monitorId: number | undefined
  let incidentId: number | undefined

  try {
    const imageRef = await step('build + push toggle-app image', () =>
      buildToggleImage({ scratchRoot: scratch, registry, onLog: (l) => log(`    ${l}`) }),
    )
    log(`  image: ${imageRef}`)

    const project = await step('create project', () =>
      createE2eProject(client, { name: `${runId}-monitoring-app`, exposedPort: 3000 }),
    )
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    const env = await step('resolve production environment', () => getProductionEnvironment(client, project.id))
    log(`  env #${env.id} (${env.name})`)

    const autoMonitor = await step(
      'production environment auto-got a default monitor (EnvironmentCreated -> ensure_monitor_for_environment)',
      async () => {
        const monitors = unwrap(
          await listMonitors({ client, path: { project_id: project.id } }),
          'listMonitors',
        )
        if (monitors.length !== 1) {
          throw new Error(`expected exactly 1 auto-created monitor for a fresh project, got ${monitors.length}`)
        }
        const m = monitors[0]!
        if (m.environment_id !== env.id) {
          throw new Error(`auto-created monitor environment_id=${m.environment_id}, expected ${env.id}`)
        }
        return m
      },
    )
    log(`  auto monitor #${autoMonitor.id} (${autoMonitor.name})`)

    const deploymentId = await step('deploy the toggle app', () =>
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

    await step('toggle app starts healthy: GET / returns 200', async () => {
      const res = await fetch(target.url, { headers: target.headers })
      if (res.status !== 200) throw new Error(`GET / returned HTTP ${res.status}, expected 200`)
    })

    const monitor = await step('create an explicit monitor via the API (covers the CRUD path)', () =>
      createMonitor({
        client,
        path: { project_id: project.id },
        body: { name: `${runId}-monitor`, monitor_type: 'http', environment_id: env.id, check_interval_seconds: 60 },
      }).then((r) => unwrap(r, 'createMonitor')),
    )
    monitorId = monitor.id
    log(`  monitor #${monitor.id} (${monitor.name})`)

    await step('project now lists both the auto and the explicit monitor', async () => {
      const monitors = unwrap(
        await listMonitors({ client, path: { project_id: project.id } }),
        'listMonitors',
      )
      const ids = monitors.map((m) => m.id).sort((a, b) => a - b)
      const expected = [autoMonitor.id, monitor.id].sort((a, b) => a - b)
      if (JSON.stringify(ids) !== JSON.stringify(expected)) {
        throw new Error(`listMonitors returned ids ${JSON.stringify(ids)}, expected ${JSON.stringify(expected)}`)
      }
    })

    await step(
      'MonitorCreated triggers an IMMEDIATE first check: current-status goes unknown -> operational within seconds',
      () =>
        pollUntil(
          () =>
            getCurrentMonitorStatus({ client, path: { monitor_id: monitor.id } }).then((r) =>
              unwrap(r, 'getCurrentMonitorStatus'),
            ),
          (s) => s.current_status === 'operational',
          { timeoutMs: 30_000, intervalMs: 2000, label: 'initial check', onPoll: (s) => log(`    ...${s.current_status}`) },
        ),
    )

    await step('current-status (no query params -- default 24h): operational, 100% uptime, a real response time', async () => {
      const s = unwrap(
        await getCurrentMonitorStatus({ client, path: { monitor_id: monitor.id } }),
        'getCurrentMonitorStatus',
      )
      if (s.current_status !== 'operational') throw new Error(`current_status="${s.current_status}", expected "operational"`)
      if (s.uptime_percentage !== 100) throw new Error(`uptime_percentage=${s.uptime_percentage}, expected 100`)
      if (typeof s.avg_response_time_ms !== 'number') {
        throw new Error(`avg_response_time_ms=${JSON.stringify(s.avg_response_time_ms)}, expected a number`)
      }
    })

    await step('uptime history (explicit range) has at least one operational data point', async () => {
      const now = new Date()
      const anHourAgo = new Date(now.getTime() - 60 * 60 * 1000)
      const history = unwrap(
        await getUptimeHistory({
          client,
          path: { monitor_id: monitor.id },
          query: { start_time: anHourAgo.toISOString(), end_time: now.toISOString() },
        }),
        'getUptimeHistory',
      )
      if (!history.uptime_data.some((d) => d.status === 'operational')) {
        throw new Error(`no operational data point in uptime history: ${JSON.stringify(history.uptime_data)}`)
      }
    })

    await step('status overview: our monitor is operational, project status is operational', async () => {
      const overview = unwrap(
        await getStatusOverview({ client, path: { project_id: project.id } }),
        'getStatusOverview',
      )
      const entry = overview.monitors.find((m) => m.monitor.id === monitor.id)
      if (!entry) throw new Error(`status overview does not include monitor ${monitor.id}`)
      if (entry.current_status !== 'operational') {
        throw new Error(`overview entry current_status="${entry.current_status}", expected "operational"`)
      }
      if (overview.status !== 'operational') {
        throw new Error(`overview.status="${overview.status}", expected "operational"`)
      }
    })

    const brokenAt = new Date()
    await step('toggle the app down: GET / now returns 500', async () => {
      const healthy = await setToggleState(target, 'down')
      if (healthy) throw new Error('toggle reported healthy=true after state=down')
      const res = await fetch(target.url, { headers: target.headers })
      if (res.status !== 500) throw new Error(`GET / returned HTTP ${res.status}, expected 500`)
    })

    await step(
      'wait for the next check cycle to catch the outage: current-status flips to "major_outage" (the raw status health_check_service.rs writes for 5xx, not the literal "down")',
      () =>
        pollUntil(
          () =>
            getCurrentMonitorStatus({ client, path: { monitor_id: monitor.id } }).then((r) =>
              unwrap(r, 'getCurrentMonitorStatus'),
            ),
          (s) => s.current_status === 'major_outage',
          {
            timeoutMs: CHECK_CYCLE_TIMEOUT_MS,
            intervalMs: 3000,
            label: 'outage detection',
            onPoll: (s) => log(`    ...${s.current_status}`),
          },
        ),
    )

    await step('a real incident was created: severity "major", status "investigating", monitor_id matches', async () => {
      const res = unwrap(
        await listIncidents({ client, path: { project_id: project.id }, query: { status: 'investigating' } }),
        'listIncidents',
      ) as { incidents: Array<{ id: number; monitor_id: number | null; severity: string; status: string }>; total: number }
      const incident = res.incidents.find((i) => i.monitor_id === monitor.id)
      if (!incident) throw new Error(`no investigating incident references monitor ${monitor.id}: ${JSON.stringify(res.incidents)}`)
      if (incident.severity !== 'major') throw new Error(`incident severity="${incident.severity}", expected "major"`)
      incidentId = incident.id
    })
    log(`  incident #${incidentId}`)

    await step(
      'bucketed status reflects the outage as "down" (regression test: down_count previously only matched the literal string "down", never the "major_outage" health_check_service.rs actually writes)',
      async () => {
        const bucketed = unwrap(
          await getBucketedStatus({
            client,
            path: { monitor_id: monitor.id },
            query: { interval: '5min', start_time: brokenAt.toISOString(), end_time: new Date().toISOString() },
          }),
          'getBucketedStatus',
        )
        const bad = bucketed.buckets.find((b) => b.down_count > 0)
        if (!bad) {
          throw new Error(`no bucket with down_count > 0: ${JSON.stringify(bucketed.buckets)}`)
        }
        if (bad.status !== 'down') {
          throw new Error(`bucket with down_count=${bad.down_count} has status="${bad.status}", expected "down"`)
        }
      },
    )

    await step('status overview: our monitor is major_outage, project status is partial_outage', async () => {
      const overview = unwrap(
        await getStatusOverview({ client, path: { project_id: project.id } }),
        'getStatusOverview',
      )
      const entry = overview.monitors.find((m) => m.monitor.id === monitor.id)
      if (!entry) throw new Error(`status overview does not include monitor ${monitor.id}`)
      if (entry.current_status !== 'major_outage') {
        throw new Error(`overview entry current_status="${entry.current_status}", expected "major_outage"`)
      }
      if (overview.status !== 'partial_outage') {
        throw new Error(`overview.status="${overview.status}", expected "partial_outage"`)
      }
    })

    await step('toggle the app back up: GET / returns 200 again', async () => {
      const healthy = await setToggleState(target, 'up')
      if (!healthy) throw new Error('toggle reported healthy=false after state=up')
      const res = await fetch(target.url, { headers: target.headers })
      if (res.status !== 200) throw new Error(`GET / returned HTTP ${res.status}, expected 200`)
    })

    await step('wait for the next check cycle to catch recovery: current-status flips back to "operational"', () =>
      pollUntil(
        () =>
          getCurrentMonitorStatus({ client, path: { monitor_id: monitor.id } }).then((r) =>
            unwrap(r, 'getCurrentMonitorStatus'),
          ),
        (s) => s.current_status === 'operational',
        {
          timeoutMs: CHECK_CYCLE_TIMEOUT_MS,
          intervalMs: 3000,
          label: 'recovery detection',
          onPoll: (s) => log(`    ...${s.current_status}`),
        },
      ),
    )

    await step('the incident auto-resolved', async () => {
      // current-status (asserted above) reads status_checks directly, so it
      // flips the instant a health check commits. Incident resolution is a
      // side effect of the SAME check processed asynchronously through the
      // job queue (StatusCheckCompleted -> OutageDetectionService), which
      // trails by anywhere from a few ms to, under heavy ambient load, most
      // of another 60s cycle -- so this has to poll, not assert once.
      const incident = await pollUntil(
        () =>
          getIncident({ client, path: { incident_id: incidentId! } }).then((r) => unwrap(r, 'getIncident')),
        (i) => i.status === 'resolved',
        {
          timeoutMs: CHECK_CYCLE_TIMEOUT_MS,
          intervalMs: 3000,
          label: 'incident auto-resolve',
          onPoll: (i) => log(`    ...${i.status}`),
        },
      )
      if (!incident.resolved_at) throw new Error('incident.resolved_at is not set')
    })

    await step('status overview back to fully operational', async () => {
      const overview = unwrap(
        await getStatusOverview({ client, path: { project_id: project.id } }),
        'getStatusOverview',
      )
      if (overview.status !== 'operational') {
        throw new Error(`overview.status="${overview.status}", expected "operational"`)
      }
    })

    await step('delete the explicit monitor: 404 afterward', async () => {
      await deleteMonitor({ client, path: { monitor_id: monitor.id } })
      const res = await getMonitor({ client, path: { monitor_id: monitor.id } })
      if (!res.error || res.response?.status !== 404) {
        throw new Error(`getMonitor after delete returned HTTP ${res.response?.status}, expected 404`)
      }
    })
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(`\n(kept resources: projects=${projectIds.join(',')} monitor=${monitorId} incident=${incidentId})`)
    } else {
      const td = await teardown(client, { deployments, projectIds })
      log(
        `\n▶ teardown: tore down ${td.teardownDeployments} deployment(s), ` +
          `deleted ${td.deletedProjects} project(s) (cascades environment + auto-monitor)` +
          (td.errors.length ? ` (${td.errors.length} errors)` : ''),
      )
      for (const e of td.errors) log(`    ! ${e}`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: MonitoringScenarioResult = { runId, ok, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ monitoring-scenario PASSED' : '❌ monitoring-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
