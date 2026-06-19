/**
 * Full end-to-end lifecycle against a live Temps instance:
 *   1. create a project
 *   2. (optional) provision a postgres service
 *   3. deploy a prebuilt image (public, or a locally-built example image)
 *   4. poll until the deployment is healthy
 *   5. resolve the public URL and smoke-check it
 *   6. fire N requests at it (load)
 *   7. verify the proxy recorded the traffic
 *   8. tear everything down (unless --keep)
 *
 * Every created resource is deleted in a finally block so a failed run still
 * leaves the instance clean.
 *
 * The per-image lifecycle is exposed as `runScenarioForImage` so other commands
 * (e.g. `examples`, which builds a source example into a local image first) can
 * reuse the exact same deploy → load → verify → teardown path.
 */
import { makeClient, resolveConfig, type TempsClientConfig } from '../lib/client.ts'
import type { Client } from '@temps-sdk/api/client'
import {
  createE2eProject,
  getProductionEnvironment,
  deployImage,
  waitForDeployment,
  waitForHttpReady,
  resolveLoadTarget,
  createE2eService,
  countProxyLogs,
  getDeployStatus,
  assertNotConsoleFallback,
  teardown,
  makeRunId,
} from '../lib/flows.ts'
import { runLoad, formatLoadResult, type LoadResult } from '../lib/load.ts'

export interface ScenarioOptions {
  image: string
  port?: string
  requests?: string
  concurrency?: string
  withDb?: boolean
  keep?: boolean
  deployTimeout?: string
  json?: boolean
  connection: { url?: string; apiKey?: string }
}

export interface StepLog {
  step: string
  ok: boolean
  detail?: string
  ms?: number
}

export interface ScenarioResult {
  runId: string
  ok: boolean
  url: string
  appUrl: string
  steps: StepLog[]
  load?: LoadResult
  proxyLogsRecorded: number
}

/** Spec for one deploy → load → verify → teardown run against a single image. */
export interface ScenarioSpec {
  /** Image ref to deploy (public or locally-built). */
  image: string
  /** Container port the image listens on. */
  port: number
  /** Number of load requests after the app is healthy. */
  requests: number
  /** Load concurrency. */
  concurrency: number
  /** HTTP path used for the readiness probe (defaults to "/"). */
  healthPath?: string
  withDb?: boolean
  keep?: boolean
  deployTimeoutMs?: number
  /** Label prefix for the run id / project name. */
  label?: string
}

/**
 * Run the deploy → load → verify → teardown lifecycle for one image and return a
 * structured result. `log` is called with human progress lines (silenced in JSON
 * mode by the caller). Never throws for an expected failure — the failure is
 * recorded in `steps` and reflected in `ok`; it only throws if teardown itself
 * cannot run.
 */
export async function runScenarioForImage(
  client: Client,
  cfg: TempsClientConfig,
  spec: ScenarioSpec,
  log: (msg: string) => void,
  onProgress?: (completed: number, total: number | undefined) => void,
): Promise<ScenarioResult> {
  const runId = makeRunId(Date.now())
  const steps: StepLog[] = []
  const projectIds: number[] = []
  const serviceIds: number[] = []
  const deployments: { projectId: number; deploymentId: number }[] = []

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

  let loadResult: LoadResult | undefined
  let recorded = 0
  let appUrl = ''

  try {
    const project = await step('create project', () =>
      createE2eProject(client, { name: `${runId}-app`, exposedPort: spec.port }),
    )
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    if (spec.withDb) {
      const svc = await step('provision postgres', () =>
        createE2eService(client, { name: `${runId}-db`, serviceType: 'postgres' }),
      )
      serviceIds.push(svc.id)
      log(`  service #${svc.id} (${svc.name})`)
    }

    const env = await step('resolve production environment', () =>
      getProductionEnvironment(client, project.id),
    )
    appUrl = env.mainUrl
    log(`  env #${env.id} (${env.name})  url=${appUrl}`)

    const deploymentId = await step('deploy image', () =>
      deployImage(client, { projectId: project.id, environmentId: env.id, imageRef: spec.image }),
    )
    deployments.push({ projectId: project.id, deploymentId })
    log(`  deployment #${deploymentId}  image=${spec.image}`)

    await step('wait for deployment', () =>
      waitForDeployment(client, {
        projectId: project.id,
        deploymentId,
        timeoutMs: spec.deployTimeoutMs ?? 300_000,
        onPoll: (s) => log(`    ...${s.state}`),
      }),
    )

    // A deployment can transiently report `running` and then flip to `failed`
    // (e.g. the image pull 404s after the route is provisioned). Re-read the
    // state and refuse to proceed on a failed deploy — otherwise the proxy's
    // catch-all console fallback answers 200 and the run looks healthy when the
    // app never started.
    await step('confirm deployment did not fail', async () => {
      const s = await getDeployStatus(client, project.id, deploymentId)
      if (!s.ok) {
        throw new Error(
          `deployment ${deploymentId} is in state "${s.state}" — the app did not start`,
        )
      }
    })

    // Resolve a target the load generator can actually reach: hit the proxy
    // origin (the instance URL) with the app's Host header. main_url is often
    // https on :443 and not directly dialable.
    if (!appUrl) throw new Error('deployment produced no public URL to hit')
    const target = resolveLoadTarget(cfg.url, appUrl)
    const healthUrl = target.url.replace(/\/$/, '') + (spec.healthPath ?? '/')
    log(`  load target ${target.url}  (Host: ${target.host})`)

    // The deployment status flipping to a terminal state doesn't guarantee the
    // container is already serving — probe HTTP (on the health path) until it
    // answers.
    await step('wait for HTTP ready', () =>
      waitForHttpReady({
        url: healthUrl,
        headers: target.headers,
        timeoutMs: 120_000,
        onPoll: (status) => log(`    ...HTTP ${status || 'unreachable'}`),
      }),
    )

    // CRITICAL: a 200 alone is not proof the app is serving — the Temps proxy
    // answers unknown/failed hosts with the console SPA (HTTP 200, HTML). Assert
    // the response is NOT that fallback so a broken deploy can never pass.
    await step('assert app is serving (not console fallback)', () =>
      assertNotConsoleFallback({ url: healthUrl, headers: target.headers }),
    )

    // Warm the container/connection pool at low concurrency so cold-start blips
    // don't pollute the measured run. Results are discarded.
    await step('warmup', () =>
      runLoad({ url: target.url, headers: target.headers, requests: 20, concurrency: 4, timeoutMs: 15_000 }),
    )

    loadResult = await step('load test', () =>
      runLoad({
        url: target.url,
        headers: target.headers,
        requests: spec.requests,
        concurrency: spec.concurrency,
        timeoutMs: 10_000,
        onProgress,
      }),
    )
    if (loadResult) log(formatLoadResult(loadResult))
    if (loadResult && !loadResult.ok) {
      throw new Error(
        `load test had ${loadResult.errors} errored requests (status codes: ${JSON.stringify(loadResult.statusCodes)})`,
      )
    }

    // Verify traffic landed. Proxy-log ingest is async and batched, so allow a
    // generous grace window (up to ~30s) before declaring the host unseen.
    await step('verify proxy logs', async () => {
      for (let i = 0; i < 20; i++) {
        recorded = await countProxyLogs(client, { host: target.logHost })
        if (recorded > 0) break
        await new Promise((r) => setTimeout(r, 1500))
      }
      if (recorded === 0) {
        throw new Error(`no proxy-log entries recorded for host ${target.logHost}`)
      }
      log(`  proxy recorded ${recorded} request(s) for ${target.logHost}`)
    })
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (spec.keep) {
      log(`\n(kept resources: projects=${projectIds.join(',')} services=${serviceIds.join(',')})`)
    } else {
      const td = await teardown(client, { deployments, projectIds, serviceIds })
      log(
        `\n▶ teardown: tore down ${td.teardownDeployments} deployment(s), ` +
          `deleted ${td.deletedProjects} project(s), ${td.deletedServices} service(s)` +
          (td.errors.length ? ` (${td.errors.length} errors)` : ''),
      )
      for (const e of td.errors) log(`    ! ${e}`)
    }
  }

  const ok = steps.every((s) => s.ok)
  return { runId, ok, url: cfg.url, appUrl, steps, load: loadResult, proxyLogsRecorded: recorded }
}

export async function scenarioCommand(opts: ScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  const onProgress = json
    ? undefined
    : (c: number, t: number | undefined) =>
        process.stderr.write(`\r    ${c}${t ? `/${t}` : ''} requests   `)

  if (!json) log(`Temps e2e scenario  ->  ${cfg.url}`)

  const result = await runScenarioForImage(
    client,
    cfg,
    {
      image: opts.image,
      port: Number(opts.port ?? '80'),
      requests: Number(opts.requests ?? '2000'),
      concurrency: Number(opts.concurrency ?? '50'),
      withDb: opts.withDb,
      keep: opts.keep,
      deployTimeoutMs: Number(opts.deployTimeout ?? '300000'),
    },
    log,
    onProgress,
  )
  if (!json) process.stderr.write('\n')

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${result.ok ? '✅ scenario PASSED' : '❌ scenario FAILED'}`)
  }
  if (!result.ok) process.exitCode = 1
}
