/**
 * Session-replay lifecycle against a live Temps instance -- proves the whole
 * ingest-then-playback chain a real rrweb-based recorder depends on, not
 * just that the query routes return 2xx:
 *
 *   1. deploy the same throwaway HTML app used by analytics-scenario
 *      (`analytics-app.ts`) and GET `/` to capture a real _temps_visitor_id
 *      cookie -- `init_session_replay` requires one and 400s without it
 *   2. POST `/api/_temps/session-replay/init` with a client-generated
 *      session id, carrying that cookie -- this genuinely RETRIES rather
 *      than asserting once: `initialize_session` looks the visitor up by
 *      cookie UUID in the `visitor` table, which is populated by the
 *      proxy's own 500ms-batched background writer (proxy_log_batch_writer.rs),
 *      so the very first attempt can race a visitor row that isn't
 *      committed yet and get 400 VisitorNotFound
 *   3. POST `/api/_temps/session-replay/events` twice with base64+zlib
 *      -compressed rrweb-shaped event batches (3 events total, spanning
 *      2000ms) -- proves the ingest path actually decodes/decompresses,
 *      not just that a well-formed request is accepted
 *   4. poll `GET /session-replays` (project list) for the new session --
 *      this list is filtered to `duration > 0`, which is only computed
 *      once events with distinct timestamps land, so an empty first batch
 *      or same-timestamp events would make the session invisible here even
 *      though it exists
 *   5. fetch the session's full event stream and assert all 3 events came
 *      back with a custom marker field intact (proves the raw JSON blob
 *      round-trips, not just the promoted timestamp/type columns) and that
 *      `duration` reflects the real timestamp span
 *   6. confirm the visitor-scoped list (`GET /visitors/{id}/session-replays`)
 *      also surfaces it -- a second, independently-implemented read path
 *   7. PUT a manual duration override and confirm it sticks via the
 *      metadata-only read -- this and step 8 use the STRING
 *      `session_replay_id`, not the numeric row id the GET routes use; the
 *      two identifier types are genuinely inconsistent across this
 *      handler's own routes, confirmed by reading the source, not assumed
 *   8. DELETE the session (soft delete) and confirm it drops out of the
 *      project list afterward
 *   9. teardown: delete the project (cascades the deployment)
 *
 * NOT covered: the legacy `store_packed_session_replay`/admin `add_events`
 * path (an authenticated alternate ingest route with its own visitor
 * lookup) -- the public `/_temps/session-replay/*` pair above is what a
 * real browser recorder actually uses, and already exercises the same
 * underlying `add_session_events`/duration-computation logic.
 */
import { deflateSync } from 'node:zlib'
import {
  getProjectSessionReplays,
  getSessionReplayEvents,
  getVisitorSessions,
  updateSessionDuration,
  getSessionReplay,
  deleteSessionReplay,
} from '@temps-sdk/api'
import type { SessionReplayInitRequest, SessionReplayEventsRequest } from '@temps-sdk/api'
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
import { buildAnalyticsAppImage, extractSetCookieValue } from '../lib/analytics-app.ts'

export interface SessionReplayScenarioOptions {
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

interface SessionReplayScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
}

/** base64(zlib(JSON array)) -- matches the zlib-format decoder the Rust side uses to unpack these batches. */
function packEvents(events: Array<Record<string, unknown>>): string {
  const json = JSON.stringify(events)
  const compressed = deflateSync(Buffer.from(json, 'utf8'))
  return compressed.toString('base64')
}

export async function sessionReplayScenarioCommand(opts: SessionReplayScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps session-replay scenario  ->  ${cfg.url}`)

  const registry = opts.registry ?? process.env.TEMPS_E2E_REGISTRY
  if (!registry) {
    throw new Error(
      'A registry is required: the analytics-app image must be pushed somewhere the server can pull ' +
        'from. Pass --registry <host:port> (e.g. localhost:5111) or set TEMPS_E2E_REGISTRY. ' +
        'Start a local one with: docker run -d -p 5111:5000 --name temps-e2e-registry registry:2',
    )
  }

  const runId = makeRunId(Date.now())
  const sessionId = crypto.randomUUID()
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
  const scratch = `${process.env.TMPDIR ?? '/tmp'}/temps-e2e-analytics-app`
  const VISITOR_UPSERT_TIMEOUT_MS = 15_000
  const LIST_VISIBILITY_TIMEOUT_MS = 15_000

  const projectIds: number[] = []
  const deployments: { projectId: number; deploymentId: number }[] = []

  try {
    const imageRef = await step('build + push analytics-app image (reused: serves real HTML so the proxy issues a visitor cookie)', () =>
      buildAnalyticsAppImage({ scratchRoot: scratch, registry, onLog: (l) => log(`    ${l}`) }),
    )
    log(`  image: ${imageRef}`)

    const project = await step('create project', () =>
      createE2eProject(client, { name: `${runId}-replay-app`, exposedPort: 3000 }),
    )
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    const env = await step('resolve production environment', () => getProductionEnvironment(client, project.id))
    log(`  env #${env.id} (${env.name})`)

    const deploymentId = await step('deploy the analytics-app', () =>
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

    let visitorCookie: string | undefined
    await step('GET / issues a real _temps_visitor_id cookie', async () => {
      const res = await fetch(target.url, { headers: target.headers })
      if (res.status !== 200) throw new Error(`GET / returned HTTP ${res.status}, expected 200`)
      visitorCookie = extractSetCookieValue(res.headers.getSetCookie(), '_temps_visitor_id')
      if (!visitorCookie) throw new Error(`no _temps_visitor_id in Set-Cookie: ${JSON.stringify(res.headers.getSetCookie())}`)
    })
    const cookieHeader = () => `_temps_visitor_id=${visitorCookie}`

    await step(
      'POST /api/_temps/session-replay/init (retries: the visitor row is upserted by a 500ms-batched background writer, not synchronously by the GET above)',
      () =>
        pollUntil(
          async () => {
            const payload: SessionReplayInitRequest = {
              sessionId,
              userAgent: 'e2e-session-replay-test/1.0',
              language: 'en-US',
              timezone: 'UTC',
              screenWidth: 1920,
              screenHeight: 1080,
              viewportWidth: 1920,
              viewportHeight: 1000,
              url: '/',
              timestamp: new Date().toISOString(),
            }
            const res = await fetch(`${target.url.replace(/\/+$/, '')}/api/_temps/session-replay/init`, {
              method: 'POST',
              headers: { ...target.headers, 'Content-Type': 'application/json', Cookie: cookieHeader() },
              body: JSON.stringify(payload),
            })
            const body = await res.json().catch(() => undefined)
            return { status: res.status, body }
          },
          (r) => r.status === 201,
          {
            timeoutMs: VISITOR_UPSERT_TIMEOUT_MS,
            intervalMs: 1000,
            label: 'session-replay init',
            onPoll: (r) => log(`    ...HTTP ${r.status}`),
          },
        ).then((r) => {
          const body = r.body as { session_id?: string } | undefined
          if (body?.session_id !== sessionId) {
            throw new Error(`init response session_id="${body?.session_id}", expected "${sessionId}"`)
          }
        }),
    )

    const t0 = Date.now()
    await step('POST /api/_temps/session-replay/events: first batch (2 events, distinct timestamps)', async () => {
      const payload: SessionReplayEventsRequest = {
        sessionId,
        events: packEvents([
          { timestamp: t0, type: 4, marker: `${runId}-a` },
          { timestamp: t0 + 1000, type: 2, marker: `${runId}-b` },
        ]),
      }
      const res = await fetch(`${target.url.replace(/\/+$/, '')}/api/_temps/session-replay/events`, {
        method: 'POST',
        headers: { ...target.headers, 'Content-Type': 'application/json', Cookie: cookieHeader() },
        body: JSON.stringify(payload),
      })
      if (res.status !== 200) throw new Error(`POST .../events (batch 1) returned HTTP ${res.status}, expected 200`)
      const body = (await res.json()) as { event_count: number }
      if (body.event_count !== 2) throw new Error(`batch 1 event_count=${body.event_count}, expected 2`)
    })

    await step('POST /api/_temps/session-replay/events: second batch (1 more event, later timestamp)', async () => {
      const payload: SessionReplayEventsRequest = {
        sessionId,
        events: packEvents([{ timestamp: t0 + 2000, type: 3, marker: `${runId}-c` }]),
      }
      const res = await fetch(`${target.url.replace(/\/+$/, '')}/api/_temps/session-replay/events`, {
        method: 'POST',
        headers: { ...target.headers, 'Content-Type': 'application/json', Cookie: cookieHeader() },
        body: JSON.stringify(payload),
      })
      if (res.status !== 200) throw new Error(`POST .../events (batch 2) returned HTTP ${res.status}, expected 200`)
      const body = (await res.json()) as { event_count: number }
      if (body.event_count !== 1) throw new Error(`batch 2 event_count=${body.event_count}, expected 1`)
    })

    const listed = await step(
      'GET /session-replays: the session appears once duration is computed (list filters duration>0)',
      () =>
        pollUntil(
          () =>
            getProjectSessionReplays({ client, query: { project_id: project.id } }).then((r) =>
              unwrap(r, 'getProjectSessionReplays'),
            ),
          (res) => res.sessions.some((s) => s.session_replay_id === sessionId),
          {
            timeoutMs: LIST_VISIBILITY_TIMEOUT_MS,
            intervalMs: 1500,
            label: 'session-replay list visibility',
            onPoll: (res) => log(`    ...${res.sessions.length} session(s) so far`),
          },
        ).then((res) => res.sessions.find((s) => s.session_replay_id === sessionId)!),
    )
    log(`  session row #${listed.id}, visitor #${listed.visitor_id}, duration=${listed.duration}ms`)

    await step('the listed session duration reflects the real 2000ms timestamp span', async () => {
      if (!listed.duration || listed.duration < 2000) {
        throw new Error(`duration=${listed.duration}, expected >= 2000`)
      }
    })

    await step('all 3 events are retrievable and their custom marker field round-tripped intact', async () => {
      const res = unwrap(
        await getSessionReplayEvents({
          client,
          path: { visitor_id: listed.visitor_id, session_id: listed.id },
        }),
        'getSessionReplayEvents',
      )
      if (res.events.length !== 3) throw new Error(`events.length=${res.events.length}, expected 3`)
      const markers = res.events
        .map((e) => (e.data as { marker?: string } | null)?.marker)
        .filter(Boolean)
        .sort()
      const expected = [`${runId}-a`, `${runId}-b`, `${runId}-c`].sort()
      if (JSON.stringify(markers) !== JSON.stringify(expected)) {
        throw new Error(`event markers ${JSON.stringify(markers)}, expected ${JSON.stringify(expected)}`)
      }
    })

    await step('the visitor-scoped list (independent read path) also surfaces the session', async () => {
      const res = unwrap(
        await getVisitorSessions({ client, path: { visitor_id: listed.visitor_id } }),
        'getVisitorSessions',
      )
      if (!res.sessions.some((s) => s.session_replay_id === sessionId)) {
        throw new Error(`visitor-scoped list does not include session ${sessionId}: ${JSON.stringify(res.sessions.map((s) => s.session_replay_id))}`)
      }
    })

    await step(
      'a manual duration override sticks (write path keys on the STRING session_replay_id, not the numeric row id)',
      async () => {
        unwrap(
          await updateSessionDuration({
            client,
            path: { visitor_id: listed.visitor_id, session_id: sessionId },
            body: { duration: 9999 },
          }),
          'updateSessionDuration',
        )
        const after = unwrap(
          await getSessionReplay({ client, path: { visitor_id: listed.visitor_id, session_id: listed.id } }),
          'getSessionReplay',
        )
        if (after.session.duration !== 9999) {
          throw new Error(`duration after override=${after.session.duration}, expected 9999`)
        }
      },
    )

    await step('DELETE the session (soft delete) drops it from the project list', async () => {
      await deleteSessionReplay({ client, path: { visitor_id: listed.visitor_id, session_id: sessionId } })
      const after = unwrap(
        await getProjectSessionReplays({ client, query: { project_id: project.id } }),
        'getProjectSessionReplays',
      )
      if (after.sessions.some((s) => s.session_replay_id === sessionId)) {
        throw new Error(`session ${sessionId} still appears in the project list after delete`)
      }
    })
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(`\n(kept resources: projects=${projectIds.join(',')} sessionId=${sessionId})`)
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
  const result: SessionReplayScenarioResult = { runId, ok, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ session-replay-scenario PASSED' : '❌ session-replay-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
