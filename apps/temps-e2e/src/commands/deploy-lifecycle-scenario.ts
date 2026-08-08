/**
 * Deployment lifecycle (rollback / pause / resume / promote) against a live
 * Temps instance -- the primary safety valve for a bad deploy on live
 * traffic. Every prior scenario in this suite only proved create -> health ->
 * teardown; this one proves the actual traffic-affecting operations work by
 * asserting the EXACT response body served over the real proxied URL at each
 * step, never just a deployment row's status field:
 *
 *   1. deploy version A of a throwaway Go app (`versioned-app.ts`) whose
 *      entire response body is a build-time-baked string -- assert live
 *      traffic serves EXACTLY "version A"
 *   2. deploy version B (a genuinely different image, same project/
 *      environment) -- assert live traffic now serves EXACTLY "version B"
 *   3. POST .../rollback to deployment A's id -- assert live traffic serves
 *      EXACTLY "version A" again (byte-for-byte, over the real proxied URL,
 *      not "the deployment row's state flipped")
 *   4. POST .../pause on the now-current (rollback) deployment -- assert the
 *      live URL genuinely stops serving the app. What "paused" actually
 *      renders as is NOT assumed going in; see the real-bug note below for
 *      what it turned out to be and why.
 *   5. POST .../resume -- assert live traffic serves "version A" again
 *   6. POST .../promote deployment B into a brand-new second environment --
 *      assert ITS live URL serves EXACTLY "version B", independent of
 *      whatever is currently live in production (proves promote is a
 *      genuinely distinct mechanism from rollback: an arbitrary historical
 *      deployment's image, copied into a different environment, not "restore
 *      a previous version in place")
 *
 * `cancel_deployment` is deliberately NOT covered here -- it aborts an
 * in-flight (pending/deploying) deployment job, a different lifecycle stage
 * than the four "deployment is already live" operations above; it doesn't
 * need a distinguishable body to verify (there is nothing serving yet to
 * assert on).
 *
 * THREE REAL PLATFORM BUGS FOUND AND FIXED while building this (see the
 * accompanying Rust commit on this branch, all in `temps-deployments` /
 * `temps-routes`):
 *
 * 1. Rollback/promote rejected their own primary use case. Both validated
 *    the SOURCE deployment's state against `["deployed", "completed"]` (or
 *    `[..., "ready"]` for promote) -- but a deployment that has since been
 *    superseded by a newer one in its own environment is "stopped" (see
 *    `cancel_previous_deployments`/`teardown_deployment`), which is exactly
 *    the state ANY deployment you'd actually want to roll back to or
 *    promote is in. Every real "rollback to the previous version" or
 *    "promote that known-good build" call 400'd with "Cannot rollback to
 *    deployment in 'stopped' state". Fixed by adding "stopped" to both
 *    allow-lists. Reproduced live: step 3 below 400'd every single run
 *    before this fix.
 *
 * 2. Pause and resume used incompatible Docker operations. `pause_deployment`
 *    used to `docker stop` AND force-`docker rm` each container, but
 *    `resume_deployment` called `deployer.resume_container` -- Docker's
 *    `unpause` (cgroup-freeze reverse), which only ever undoes a genuine
 *    `docker pause`. Nothing in the real pause path ever paused (froze) a
 *    container; it removed it outright, so resume always failed against a
 *    real deployment ("no such container") the moment pause had actually
 *    run. Fixed by having pause only `docker stop` (never remove), and
 *    resume call `deployer.start_container` (the correct reverse of
 *    `stop`) instead of `resume_container`.
 *
 * 3. Even with (2) fixed, the paused container stayed live in the proxy's
 *    route table indefinitely. `route_table::load_routes` filtered
 *    candidate upstream containers ONLY on `deleted_at IS NULL`, never on
 *    `status` -- so a stopped-but-not-removed container's row still looked
 *    "routable". Worse, NEITHER `pause_deployment` NOR `resume_deployment`
 *    ever requested a route-table reload: the only DB triggers wired to the
 *    in-process route-table listener are on `environments`/`projects`
 *    (see the `m2025*_add_*_route_trigger.rs` migrations), and a bare
 *    `deployment_containers.status` UPDATE fires neither. Reproduced live:
 *    after pause, the proxy kept retrying the OLD (still "valid-looking")
 *    container address and returned Pingora's own `503 Service Unavailable`
 *    ("Fail to connect ... Connection refused") -- not a clean "paused"
 *    signal, just an accident of a stale cached route. Fixed by (a) filtering
 *    `route_table::load_routes` to `status IS NULL OR status = 'running'`,
 *    so a stopped container's route is skipped entirely once reloaded, and
 *    (b) having pause/resume publish `Job::ForceRouteReload` (the same
 *    in-process broadcast `mark_deployment_complete.rs` already uses after a
 *    normal deploy) so the reload actually happens immediately instead of
 *    waiting on an unrelated route change. With both fixes, pausing makes
 *    the route disappear entirely and the proxy falls through to its
 *    existing unknown-host console-fallback response (HTTP 200,
 *    `<title>Temps</title>`) -- that fallback is therefore the real,
 *    asserted "paused" behavior in step 4 below, discovered by observing
 *    actual proxy logs, not assumed.
 */
import { createEnvironment, pauseDeployment, promoteDeployment, resumeDeployment, rollbackToDeployment } from '@temps-sdk/api'
import { makeClient, resolveConfig, unwrap } from '../lib/client.ts'
import {
  createE2eProject,
  getProductionEnvironment,
  deployImage,
  waitForDeployment,
  getDeployStatus,
  waitForHttpReady,
  assertNotConsoleFallback,
  waitForConsoleFallback,
  fetchBody,
  resolveLoadTarget,
  teardown,
  makeRunId,
} from '../lib/flows.ts'
import { buildVersionedAppImage, VERSIONED_APP_PORT } from '../lib/versioned-app.ts'

export interface DeployLifecycleScenarioOptions {
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

interface DeployLifecycleScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
}

/** Poll a target until its EXACT body matches `expected`, or time out. */
async function waitForExactBody(
  target: { url: string; headers?: Record<string, string> },
  expected: string,
  opts: { timeoutMs?: number; intervalMs?: number; label: string },
): Promise<void> {
  const timeoutMs = opts.timeoutMs ?? 60_000
  const intervalMs = opts.intervalMs ?? 1500
  const start = performance.now()
  let last = { status: 0, body: '' }
  while (performance.now() - start < timeoutMs) {
    try {
      last = await fetchBody(target.url, target.headers, 10_000)
      if (last.body === expected) return
    } catch {
      // transient -- retry
    }
    await sleep(intervalMs)
  }
  throw new Error(
    `${opts.label}: expected EXACT body ${JSON.stringify(expected)}, got HTTP ${last.status} ` +
      `${JSON.stringify(last.body.slice(0, 120))} after ${Math.round(timeoutMs / 1000)}s`,
  )
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms))
}

export async function deployLifecycleScenarioCommand(
  opts: DeployLifecycleScenarioOptions,
): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps deploy-lifecycle scenario  ->  ${cfg.url}`)

  const registry = opts.registry ?? process.env.TEMPS_E2E_REGISTRY
  if (!registry) {
    throw new Error(
      'A registry is required: the versioned-app images must be pushed somewhere the server can ' +
        'pull from. Pass --registry <host:port> (e.g. localhost:5111) or set TEMPS_E2E_REGISTRY. ' +
        'Start a local one with: docker run -d -p 5111:5000 --name temps-e2e-registry registry:2',
    )
  }

  const runId = makeRunId(Date.now())
  const deployTimeoutMs = Number(opts.deployTimeout ?? '300000')
  const scratch = `${process.env.TMPDIR ?? '/tmp'}/temps-e2e-deploy-lifecycle`
  const versionA = `TEMPS-E2E-LIFECYCLE-A-${runId}`
  const versionB = `TEMPS-E2E-LIFECYCLE-B-${runId}`

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
  const deployments: { projectId: number; deploymentId: number }[] = []
  let stagingEnvId: number | undefined

  try {
    const project = await step('create project', () =>
      createE2eProject(client, { name: `${runId}-app`, exposedPort: VERSIONED_APP_PORT }),
    )
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    const env = await step('resolve production environment', () =>
      getProductionEnvironment(client, project.id),
    )
    log(`  env #${env.id} (${env.name})  url=${env.mainUrl}`)
    const target = resolveLoadTarget(cfg.url, env.mainUrl)

    const imageA = await step('build + push version A image', () =>
      buildVersionedAppImage({ scratchRoot: scratch, registry, version: versionA, onLog: (l) => log(`    ${l}`) }),
    )
    log(`  image A: ${imageA}`)

    const imageB = await step('build + push version B image', () =>
      buildVersionedAppImage({ scratchRoot: scratch, registry, version: versionB, onLog: (l) => log(`    ${l}`) }),
    )
    log(`  image B: ${imageB}`)

    // --- 1. deploy version A, assert live traffic serves it exactly ---
    const deploymentAId = await step('deploy version A', () =>
      deployImage(client, { projectId: project.id, environmentId: env.id, imageRef: imageA }),
    )
    deployments.push({ projectId: project.id, deploymentId: deploymentAId })
    log(`  deployment #${deploymentAId}`)

    await step('wait for deployment A', () =>
      waitForDeployment(client, {
        projectId: project.id,
        deploymentId: deploymentAId,
        timeoutMs: deployTimeoutMs,
        onPoll: (s) => log(`    ...${s.state}`),
      }),
    )
    await step('confirm deployment A did not fail', async () => {
      const s = await getDeployStatus(client, project.id, deploymentAId)
      if (!s.ok) throw new Error(`deployment ${deploymentAId} is in state "${s.state}"`)
    })
    await step('wait for HTTP ready (A)', () =>
      waitForHttpReady({ url: target.url, headers: target.headers, timeoutMs: 120_000 }),
    )
    await step('assert live traffic serves version A EXACTLY', () =>
      waitForExactBody(target, versionA, { label: 'production after deploy A' }),
    )

    // --- 2. deploy version B to the SAME project/environment ---
    const deploymentBId = await step('deploy version B', () =>
      deployImage(client, { projectId: project.id, environmentId: env.id, imageRef: imageB }),
    )
    deployments.push({ projectId: project.id, deploymentId: deploymentBId })
    log(`  deployment #${deploymentBId}`)

    await step('wait for deployment B', () =>
      waitForDeployment(client, {
        projectId: project.id,
        deploymentId: deploymentBId,
        timeoutMs: deployTimeoutMs,
        onPoll: (s) => log(`    ...${s.state}`),
      }),
    )
    await step('confirm deployment B did not fail', async () => {
      const s = await getDeployStatus(client, project.id, deploymentBId)
      if (!s.ok) throw new Error(`deployment ${deploymentBId} is in state "${s.state}"`)
    })
    await step('assert live traffic serves version B EXACTLY', () =>
      waitForExactBody(target, versionB, { label: 'production after deploy B' }),
    )

    // --- 3. rollback to deployment A, assert traffic reverts EXACTLY ---
    const rollbackDeploymentId = await step('rollback to deployment A', async () => {
      const res = unwrap(
        await rollbackToDeployment({
          client,
          path: { project_id: project.id, deployment_id: deploymentAId },
        }),
        'rollbackToDeployment',
      )
      return res.id
    })
    deployments.push({ projectId: project.id, deploymentId: rollbackDeploymentId })
    log(`  rollback deployment #${rollbackDeploymentId}`)

    await step('wait for rollback deployment', () =>
      waitForDeployment(client, {
        projectId: project.id,
        deploymentId: rollbackDeploymentId,
        timeoutMs: deployTimeoutMs,
        onPoll: (s) => log(`    ...${s.state}`),
      }),
    )
    await step('confirm rollback did not fail', async () => {
      const s = await getDeployStatus(client, project.id, rollbackDeploymentId)
      if (!s.ok) throw new Error(`rollback deployment ${rollbackDeploymentId} is in state "${s.state}"`)
    })
    await step('assert live traffic reverted to version A EXACTLY (real rollback)', () =>
      waitForExactBody(target, versionA, { label: 'production after rollback' }),
    )

    // --- 4. pause the now-current (rollback) deployment ---
    await step('pause the current deployment', async () => {
      unwrap(
        await pauseDeployment({
          client,
          path: { project_id: project.id, deployment_id: rollbackDeploymentId },
        }),
        'pauseDeployment',
      )
    })
    await step('assert live traffic actually stopped (real paused-state behavior)', () =>
      waitForConsoleFallback({ url: target.url, headers: target.headers, timeoutMs: 45_000 }),
    )

    // --- 5. resume, assert traffic comes back exactly as before ---
    await step('resume the deployment', async () => {
      unwrap(
        await resumeDeployment({
          client,
          path: { project_id: project.id, deployment_id: rollbackDeploymentId },
        }),
        'resumeDeployment',
      )
    })
    await step('assert live traffic resumed serving version A EXACTLY (real resume)', () =>
      waitForExactBody(target, versionA, { label: 'production after resume', timeoutMs: 90_000 }),
    )

    // --- 6. promote deployment B into a brand-new second environment ---
    const staging = await step('create staging environment', async () => {
      const res = unwrap(
        await createEnvironment({
          client,
          path: { project_id: project.id },
          body: { name: `staging-${runId}`, branch: `staging-${runId}` },
        }),
        'createEnvironment',
      )
      return { id: res.id, mainUrl: res.main_url }
    })
    stagingEnvId = staging.id
    log(`  staging env #${staging.id}  url=${staging.mainUrl}`)
    const stagingTarget = resolveLoadTarget(cfg.url, staging.mainUrl)

    const promotedDeploymentId = await step('promote deployment B into staging', async () => {
      const res = unwrap(
        await promoteDeployment({
          client,
          path: { project_id: project.id, deployment_id: deploymentBId },
          body: { target_environment_id: staging.id },
        }),
        'promoteDeployment',
      )
      return res.id
    })
    deployments.push({ projectId: project.id, deploymentId: promotedDeploymentId })
    log(`  promoted deployment #${promotedDeploymentId}`)

    await step('wait for promoted deployment', () =>
      waitForDeployment(client, {
        projectId: project.id,
        deploymentId: promotedDeploymentId,
        timeoutMs: deployTimeoutMs,
        onPoll: (s) => log(`    ...${s.state}`),
      }),
    )
    await step('confirm promotion did not fail', async () => {
      const s = await getDeployStatus(client, project.id, promotedDeploymentId)
      if (!s.ok) throw new Error(`promoted deployment ${promotedDeploymentId} is in state "${s.state}"`)
    })
    await step('wait for HTTP ready (staging)', () =>
      waitForHttpReady({ url: stagingTarget.url, headers: stagingTarget.headers, timeoutMs: 120_000 }),
    )
    await step('assert not console fallback (staging)', () =>
      assertNotConsoleFallback({ url: stagingTarget.url, headers: stagingTarget.headers }),
    )
    await step(
      'assert staging serves version B EXACTLY, independent of production (real promote, not rollback)',
      () => waitForExactBody(stagingTarget, versionB, { label: 'staging after promote' }),
    )
    await step(
      'assert production is UNCHANGED by the promote (still version A)',
      async () => {
        const r = await fetchBody(target.url, target.headers)
        if (r.body !== versionA) {
          throw new Error(
            `promote leaked into production: expected ${JSON.stringify(versionA)}, got ${JSON.stringify(r.body.slice(0, 120))}`,
          )
        }
      },
    )
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(`\n(kept resources: projects=${projectIds.join(',')} staging_env=${stagingEnvId ?? '-'})`)
    } else {
      // No explicit environment delete: `DELETE .../environments/{id}` is
      // gated behind `require_sensitive_action` (browser-session-only, see
      // temps-auth/src/sensitive_action.rs -- an API key is deliberately
      // denied), so it can never succeed from this API-key-driven harness.
      // `deleteProject` below cascades environments (and their deployments)
      // via `ON DELETE CASCADE` regardless -- the same path every other
      // scenario already relies on -- so the staging environment created
      // above is cleaned up as part of normal project teardown, not here.
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
  const result: DeployLifecycleScenarioResult = { runId, ok, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ deploy-lifecycle-scenario PASSED' : '❌ deploy-lifecycle-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
