/**
 * Web-analytics lifecycle against a live Temps instance -- proves the whole
 * chain a real tracking snippet depends on, not just that the query routes
 * return 2xx:
 *
 *   1. deploy a throwaway Go app (`analytics-app.ts`) that serves a real
 *      `text/html` page at `/` -- required because `should_track_page`
 *      (temps-proxy) only issues visitor/session cookies for HTML responses
 *      (or 4xx/5xx); the other apps in this suite all answer plain text and
 *      would never trigger cookie issuance at all
 *   2. confirm the fresh project reports has_events=false before anything
 *      is sent
 *   3. GET `/` through the proxy and capture the REAL `_temps_visitor_id` /
 *      `_temps_sid` Set-Cookie values it issues -- these are encrypted,
 *      proxy-minted tokens; there's no way to synthesize them from the
 *      client side
 *   4. POST `/api/_temps/event` twice with those SAME cookies (a pageview,
 *      then a custom event carrying a `code`-like custom field) -- the
 *      unauthenticated public ingest path a real browser tracking snippet
 *      uses, resolved to a project purely via the Host header
 *   5. poll `GET /analytics/event-entries` for the custom event and assert
 *      its custom `event_data` round-tripped into the queryable `props`
 *      JSON, and that it resolved to a numeric visitor_id (the proxy's
 *      visitor/session upsert is a 500ms-batched background writer, so this
 *      genuinely polls rather than asserting once)
 *   6. call `GET /analytics/visitors/{visitor_id}/journey` and assert
 *      total_sessions=1, total_events=2, and BOTH event names appear in the
 *      same session -- proving the two independent POSTs were stitched into
 *      one session purely from the shared cookie pair, the analytics
 *      equivalent of error-tracking's fingerprint-grouping proof
 *   7. teardown: delete the project (cascades the deployment)
 *
 * Real platform gap found while building this: `packages/api/openapi.json`
 * (the source for `@temps-sdk/api`, which this suite depends on) had
 * drifted to 590 of the live server's 674 paths -- the entire
 * `temps-analytics` query surface (event-entries, has-events,
 * visitor-journey, ...) was unreachable from any TypeScript consumer. Fixed
 * in a preceding commit by regenerating the SDK from a live server's spec;
 * see that commit for detail. Not a bug in the Rust handlers themselves --
 * every route here was already fully wired and working, just unreachable
 * from generated clients.
 *
 * NOT covered: server-side ingestion (`POST /projects/{id}/events/ingest`,
 * used by app backends forwarding already-authenticated cookies) and the
 * segment-filter/attribution query surface (referrer/UTM/country
 * breakdowns). Both are real, but exercising them meaningfully needs either
 * a second authenticated actor forwarding real encrypted cookies (out of
 * scope for a single-actor scenario) or synthetic geolocation/referrer
 * headers whose attribution logic is better covered by the existing Rust
 * unit tests than by another live HTTP round trip.
 */
import { checkAnalyticsHasEvents, getEventEntries, getVisitorJourney } from '@temps-sdk/api'
import type { EventMetricsPayload } from '@temps-sdk/api'
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
  BROWSER_DOCUMENT_HEADERS,
  teardown,
  makeRunId,
  pollUntil,
} from '../lib/flows.ts'
import { buildAnalyticsAppImage, extractSetCookieValue } from '../lib/analytics-app.ts'

export interface AnalyticsScenarioOptions {
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

interface AnalyticsScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
}

export async function analyticsScenarioCommand(opts: AnalyticsScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps analytics scenario  ->  ${cfg.url}`)

  const registry = opts.registry ?? process.env.TEMPS_E2E_REGISTRY
  if (!registry) {
    throw new Error(
      'A registry is required: the analytics-app image must be pushed somewhere the server can pull ' +
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
  const scratch = `${process.env.TMPDIR ?? '/tmp'}/temps-e2e-analytics-app`
  // Visitor/session upsert is a 500ms-batched background writer (see
  // proxy_log_batch_writer.rs's FLUSH_INTERVAL) -- comfortably shorter than
  // the check-cycle waits elsewhere in this suite, but still genuinely async.
  const TRACKING_FLUSH_TIMEOUT_MS = 30_000

  const projectIds: number[] = []
  const deployments: { projectId: number; deploymentId: number }[] = []

  try {
    const imageRef = await step('build + push analytics-app image', () =>
      buildAnalyticsAppImage({ scratchRoot: scratch, registry, onLog: (l) => log(`    ${l}`) }),
    )
    log(`  image: ${imageRef}`)

    const project = await step('create project', () =>
      createE2eProject(client, { name: `${runId}-analytics-app`, exposedPort: 3000 }),
    )
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    const env = await step('resolve production environment', () => getProductionEnvironment(client, project.id))
    log(`  env #${env.id} (${env.name})`)

    await step('fresh project reports has_events=false', async () => {
      const res = unwrap(
        await checkAnalyticsHasEvents({ client, query: { project_id: project.id } }),
        'checkAnalyticsHasEvents',
      )
      if (res.has_events) throw new Error('has_events=true for a project with no deploy yet')
    })

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
    let sessionCookie: string | undefined
    await step(
      'GET / issues real _temps_visitor_id and _temps_sid cookies (should_track_page requires an HTML response AND browser document-navigation headers)',
      async () => {
        const res = await fetch(target.url, {
          headers: { ...target.headers, ...BROWSER_DOCUMENT_HEADERS },
        })
        if (res.status !== 200) throw new Error(`GET / returned HTTP ${res.status}, expected 200`)
        const setCookies = res.headers.getSetCookie()
        visitorCookie = extractSetCookieValue(setCookies, '_temps_visitor_id')
        sessionCookie = extractSetCookieValue(setCookies, '_temps_sid')
        if (!visitorCookie) throw new Error(`no _temps_visitor_id in Set-Cookie: ${JSON.stringify(setCookies)}`)
        if (!sessionCookie) throw new Error(`no _temps_sid in Set-Cookie: ${JSON.stringify(setCookies)}`)
      },
    )

    // The other half of the contract. #700 narrowed tracking so non-browser
    // traffic stops minting a visitor/session on every request; the step above
    // only proves the positive case still works. These pin the negative, so a
    // future loosening of should_track_page can't silently reintroduce the
    // churn while the happy path keeps passing.
    const assertNotTracked = async (what: string, headers: Record<string, string>) => {
      const res = await fetch(target.url, { headers: { ...target.headers, ...headers } })
      if (res.status !== 200) throw new Error(`GET / returned HTTP ${res.status}, expected 200`)
      const setCookies = res.headers.getSetCookie()
      const leaked = ['_temps_visitor_id', '_temps_sid'].filter((name) =>
        extractSetCookieValue(setCookies, name),
      )
      if (leaked.length > 0) {
        throw new Error(`${what} was tracked; unexpected ${leaked.join(', ')} in ${JSON.stringify(setCookies)}`)
      }
    }

    await step('a generic HTTP client (wildcard Accept) is NOT tracked', () =>
      assertNotTracked('generic HTTP client', { Accept: '*/*' }),
    )

    // A browser data fetch DOES send Fetch Metadata, and must be rejected even
    // though it asks for HTML — the case a plain-Accept heuristic would miss.
    await step('a browser data fetch (Sec-Fetch-Dest: empty) is NOT tracked', () =>
      assertNotTracked('browser data fetch', {
        Accept: 'text/html,application/xhtml+xml',
        'Sec-Fetch-Dest': 'empty',
        'Sec-Fetch-Mode': 'cors',
      }),
    )

    const cookieHeader = () => `_temps_visitor_id=${visitorCookie}; _temps_sid=${sessionCookie}`
    const customEventName = `${runId}-purchase`

    await step('POST /api/_temps/event: a pageview, carrying the captured cookies', async () => {
      const payload: EventMetricsPayload = {
        event_name: 'pageview',
        event_data: {},
        request_path: '/',
        request_query: '',
        screen_width: 1920,
        screen_height: 1080,
        viewport_width: 1920,
        viewport_height: 1000,
        language: 'en-US',
        page_title: 'Analytics E2E App',
        referrer: null,
      }
      const res = await fetch(`${target.url.replace(/\/+$/, '')}/api/_temps/event`, {
        method: 'POST',
        headers: { ...target.headers, 'Content-Type': 'application/json', Cookie: cookieHeader() },
        body: JSON.stringify(payload),
      })
      if (res.status !== 204) throw new Error(`POST /api/_temps/event (pageview) returned HTTP ${res.status}, expected 204`)
    })

    await step('POST /api/_temps/event: a custom event with a custom field, SAME cookies', async () => {
      const payload: EventMetricsPayload = {
        event_name: customEventName,
        event_data: { plan: 'pro', amount: 42 },
        request_path: '/',
        request_query: '',
        language: 'en-US',
        referrer: null,
      }
      const res = await fetch(`${target.url.replace(/\/+$/, '')}/api/_temps/event`, {
        method: 'POST',
        headers: { ...target.headers, 'Content-Type': 'application/json', Cookie: cookieHeader() },
        body: JSON.stringify(payload),
      })
      if (res.status !== 204) throw new Error(`POST /api/_temps/event (custom) returned HTTP ${res.status}, expected 204`)
    })

    const nowIso = () => new Date().toISOString()
    const startIso = new Date(Date.now() - 5 * 60_000).toISOString()

    const entry = await step(
      'event-entries: the custom event is queryable, its custom event_data round-tripped, and it resolved to a numeric visitor_id',
      () =>
        pollUntil(
          () =>
            getEventEntries({
              client,
              query: { project_id: project.id, event_name: customEventName, start_date: startIso, end_date: nowIso() },
            }).then((r) => unwrap(r, 'getEventEntries')),
          (res) => res.total_count >= 1 && res.entries[0]?.visitor_id != null,
          {
            timeoutMs: TRACKING_FLUSH_TIMEOUT_MS,
            intervalMs: 2000,
            label: 'event-entries visitor_id resolution',
            onPoll: (res) => log(`    ...total_count=${res.total_count} visitor_id=${res.entries[0]?.visitor_id}`),
          },
        ).then((res) => res.entries[0]!),
    )

    await step('the custom event_data (plan/amount) round-tripped into props', async () => {
      const props = entry.props as { plan?: string; amount?: number } | null | undefined
      if (props?.plan !== 'pro') throw new Error(`entry.props.plan="${props?.plan}", expected "pro"`)
      if (props?.amount !== 42) throw new Error(`entry.props.amount=${props?.amount}, expected 42`)
    })

    await step('the event carries a real session_id from the cookie', async () => {
      if (!entry.session_id) throw new Error('entry.session_id is empty — event was not attributed to a session')
    })

    await step(
      'visitor journey: both events (pageview + custom) landed in ONE session — proves cookie-based session stitching',
      async () => {
        const journey = unwrap(
          await getVisitorJourney({
            client,
            path: { visitor_id: entry.visitor_id! },
            query: { project_id: project.id },
          }),
          'getVisitorJourney',
        )
        if (journey.total_sessions !== 1) {
          throw new Error(`total_sessions=${journey.total_sessions}, expected 1 (both POSTs shared one cookie pair)`)
        }
        if (journey.total_events !== 2) {
          throw new Error(`total_events=${journey.total_events}, expected 2 (pageview + custom)`)
        }
        const names = journey.sessions[0]?.events.map((e) => e.event_name) ?? []
        if (!names.includes('pageview')) throw new Error(`session events ${JSON.stringify(names)} missing "pageview"`)
        if (!names.includes(customEventName)) {
          throw new Error(`session events ${JSON.stringify(names)} missing "${customEventName}"`)
        }
      },
    )

    await step('has_events is now true', async () => {
      const res = unwrap(
        await checkAnalyticsHasEvents({ client, query: { project_id: project.id } }),
        'checkAnalyticsHasEvents',
      )
      if (!res.has_events) throw new Error('has_events=false after real events were recorded')
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
  const result: AnalyticsScenarioResult = { runId, ok, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ analytics-scenario PASSED' : '❌ analytics-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
