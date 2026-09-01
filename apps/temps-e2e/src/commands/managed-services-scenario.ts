// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Managed-services (external-services) lifecycle against a live Temps
 * instance -- closing a real gap: `scenario --with-db` already provisions a
 * postgres service but never links it to the project or verifies it's
 * usable, so "a managed database attached to a project" had ZERO real
 * end-to-end coverage before this.
 *
 *   1. provision a real standalone postgres service
 *   2. link it to a project BEFORE deploying -- env vars are resolved once
 *      at deploy-job-creation time (workflow_planner.rs), so linking after a
 *      deploy already exists would not inject anything into that container
 *   3. deploy a throwaway Go app (`probe-app.ts`) that connects with
 *      whatever `POSTGRES_URL` the deployment injects, creates a table, and
 *      on every `/probe` hit does a real INSERT + `SELECT count(*)`
 *   4. hit `/probe` through the real proxy K times and assert the exact
 *      returned count matches K -- there is no SQL-exec endpoint anywhere in
 *      the platform API, so this is the only way to prove the injected
 *      connection string is genuinely writable, not just present
 *   5. assert the resolved env vars list carries a real `POSTGRES_URL`
 *      sourced from this exact service (not a manual override), and that
 *      the real (unmasked) value is a well-formed `postgresql://` URI --
 *      `GET .../env-vars/resolved` itself always masks to "***", the
 *      dedicated `.../resolved/{key}/value` reveal endpoint is what returns
 *      the real string
 *   6. unlink the service and assert POSTGRES_URL disappears from the
 *      resolved list
 *   7. teardown (deployment, project, service)
 *
 * Real findings while building this:
 *  - the postgres provider injects `POSTGRES_URL` (plus
 *    POSTGRES_HOST/PORT/DB/USER/PASSWORD), NOT `DATABASE_URL` -- that key is
 *    what mariadb/mongodb inject. An app written against the wrong key
 *    silently gets nothing and fails at boot with no connection.
 *  - `scenario --with-db` was ALREADY BROKEN before this file existed:
 *    `PostgresParameterStrategy::validate_for_creation` (crates/temps-
 *    providers/src/parameter_strategies.rs) requires `database`/`username`
 *    parameters with no defaults, but `scenario.ts`'s `createE2eService`
 *    call passed none -- every `--with-db` run 400'd on service creation.
 *    Fixed there too (same one-line default), alongside this file.
 */
import {
  linkServiceToProject,
  unlinkServiceFromProject,
  listProjectServices,
  getResolvedEnvironmentVariables,
  getResolvedEnvironmentVariableValue,
  getService,
} from '@temps-sdk/api'
import { makeClient, resolveConfig, unwrap } from '../lib/client.ts'
import {
  createE2eProject,
  createE2eService,
  getProductionEnvironment,
  deployImage,
  waitForDeployment,
  getDeployStatus,
  waitForHttpReady,
  assertNotConsoleFallback,
  resolveLoadTarget,
  teardown,
  makeRunId,
} from '../lib/flows.ts'
import { buildProbeImage, PROBE_APP_HEALTH_PATH } from '../lib/probe-app.ts'

export interface ManagedServicesScenarioOptions {
  registry?: string
  probeRequests?: string
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

interface ManagedServicesScenarioResult {
  runId: string
  ok: boolean
  probeCount?: number
  steps: StepLog[]
}

export async function managedServicesScenarioCommand(opts: ManagedServicesScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps managed-services scenario  ->  ${cfg.url}`)

  const registry = opts.registry ?? process.env.TEMPS_E2E_REGISTRY
  if (!registry) {
    throw new Error(
      'A registry is required: the probe app image must be pushed somewhere the server can pull ' +
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
  const probeRequests = Number(opts.probeRequests ?? '5')
  const scratch = `${process.env.TMPDIR ?? '/tmp'}/temps-e2e-probe`

  const projectIds: number[] = []
  const serviceIds: number[] = []
  const deployments: { projectId: number; deploymentId: number }[] = []
  let probeCount: number | undefined

  try {
    const imageRef = await step('build + push db-probe image', () =>
      buildProbeImage({ scratchRoot: scratch, registry, onLog: (l) => log(`    ${l}`) }),
    )
    log(`  image: ${imageRef}`)

    const service = await step('provision a real standalone postgres service', () =>
      // `database`/`username` are required by PostgresParameterStrategy::validate_for_creation
      // (crates/temps-providers/src/parameter_strategies.rs) -- omitting them 400s. This is the
      // container's own default database, distinct from the per-tenant one `get_runtime_env_vars`
      // auto-creates per project+environment (`{project_id}_{environment}`) that the probe app
      // actually connects to.
      createE2eService(client, {
        name: `${runId}-pg`,
        serviceType: 'postgres',
        parameters: { database: 'app', username: 'app' },
      }),
    )
    serviceIds.push(service.id)
    log(`  service #${service.id}`)

    await step('confirm service status is running (standalone create awaits container bring-up)', async () => {
      const detail = unwrap(await getService({ client, path: { id: service.id } }), 'getService')
      if (detail.service.status !== 'running') {
        throw new Error(`service ${service.id} status="${detail.service.status}", expected "running"`)
      }
    })

    const project = await step('create project', () =>
      createE2eProject(client, { name: `${runId}-svc-app`, exposedPort: 3000 }),
    )
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    await step('link service to project BEFORE deploying (env vars resolve at deploy-job-creation time)', async () => {
      unwrap(
        await linkServiceToProject({ client, path: { id: service.id }, body: { project_id: project.id } }),
        'linkServiceToProject',
      )
    })

    await step('list project services: contains exactly the linked service', async () => {
      const linked = unwrap(
        await listProjectServices({ client, path: { project_id: project.id } }),
        'listProjectServices',
      )
      const ids = linked.map((s) => s.service.id)
      if (JSON.stringify(ids) !== JSON.stringify([service.id])) {
        throw new Error(`listProjectServices returned service ids ${JSON.stringify(ids)}, expected [${service.id}]`)
      }
    })

    const env = await step('resolve production environment', () =>
      getProductionEnvironment(client, project.id),
    )
    log(`  env #${env.id} (${env.name})`)

    const deploymentId = await step('deploy the db-probe app', () =>
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

    await step('health check reports a real DB ping succeeding', async () => {
      const res = await fetch(`${target.url.replace(/\/+$/, '')}${PROBE_APP_HEALTH_PATH}`, {
        headers: target.headers,
      })
      if (res.status !== 200) {
        throw new Error(`GET ${PROBE_APP_HEALTH_PATH} returned HTTP ${res.status} -- POSTGRES_URL likely missing/unreachable`)
      }
    })

    await step(`hit /probe ${probeRequests}x through the real proxy: exact row-count round trip`, async () => {
      let last = 0
      for (let i = 0; i < probeRequests; i++) {
        const res = await fetch(`${target.url.replace(/\/+$/, '')}/probe`, { headers: target.headers })
        if (res.status !== 200) throw new Error(`GET /probe returned HTTP ${res.status}`)
        const body = (await res.json()) as { count: number }
        if (body.count !== last + 1) {
          throw new Error(`/probe #${i + 1} returned count=${body.count}, expected ${last + 1} (monotonic)`)
        }
        last = body.count
      }
      if (last !== probeRequests) {
        throw new Error(`final /probe count=${last}, expected exactly ${probeRequests}`)
      }
      probeCount = last
    })

    await step('resolved env vars: POSTGRES_URL is present, sourced from this exact service', async () => {
      const resolved = unwrap(
        await getResolvedEnvironmentVariables({ client, path: { project_id: project.id } }),
        'getResolvedEnvironmentVariables',
      )
      const row = resolved.find((r) => r.key === 'POSTGRES_URL')
      if (!row) {
        throw new Error(`no POSTGRES_URL in resolved env vars: ${JSON.stringify(resolved.map((r) => r.key))}`)
      }
      if (row.source.type !== 'integration' || row.source.service.service_id !== service.id) {
        throw new Error(`POSTGRES_URL source=${JSON.stringify(row.source)}, expected integration/service #${service.id}`)
      }
    })

    await step('reveal endpoint returns a real, well-formed postgresql:// URI (not "***")', async () => {
      const revealed = unwrap(
        await getResolvedEnvironmentVariableValue({
          client,
          path: { project_id: project.id, key: 'POSTGRES_URL' },
        }),
        'getResolvedEnvironmentVariableValue',
      )
      if (revealed.value === '***' || !revealed.value) {
        throw new Error(`resolved POSTGRES_URL value is "${revealed.value}", expected a real connection string`)
      }
      if (!/^postgresql:\/\/[^:]+:[^@]+@[^:/]+:\d+\/\S+$/.test(revealed.value)) {
        throw new Error(`resolved POSTGRES_URL value does not look like a real postgres URI: "${revealed.value}"`)
      }
    })

    await step('unlink: POSTGRES_URL disappears from the resolved list', async () => {
      await unlinkServiceFromProject({ client, path: { id: service.id, project_id: project.id } })
      const resolved = unwrap(
        await getResolvedEnvironmentVariables({ client, path: { project_id: project.id } }),
        'getResolvedEnvironmentVariables',
      )
      if (resolved.some((r) => r.key === 'POSTGRES_URL')) {
        throw new Error('POSTGRES_URL still present in resolved env vars after unlinking the service')
      }
    })
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
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

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: ManagedServicesScenarioResult = { runId, ok, probeCount, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ managed-services-scenario PASSED' : '❌ managed-services-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
