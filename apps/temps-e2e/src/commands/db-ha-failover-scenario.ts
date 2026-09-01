// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Postgres HA cluster (pg_auto_failover) automatic-failover proof against a
 * live Temps instance -- closing a real gap: `PostgresClusterService`
 * (crates/temps-providers/src/externalsvc/postgres_cluster.rs), the
 * `cluster-health`/`members`/`promote` API surface (crates/temps-providers/
 * src/handlers/handlers.rs), and the multi-host `POSTGRES_URL` env-var
 * injection (`ExternalServiceManager::build_cluster_env_vars_for_resource`)
 * are all real and wired end-to-end, but had ZERO e2e coverage before this.
 * This is single-Docker-host HA (every member is `node_id: null`, placed on
 * the control plane with unique container names/ports) -- distinct from
 * multi-node/WireGuard clustering, which needs separate real hosts and is
 * out of scope here.
 *
 *   1. provision a real 1-monitor + 2-data-node Postgres HA cluster
 *      (`topology: 'cluster'`) and poll until the service and all 3 members
 *      report `status: 'running'`
 *   2. poll `GET /external-services/{id}/cluster-health` (reads
 *      `pgautofailover.node` directly off the monitor, TLS, autoctl_node)
 *      until the cluster reaches a steady state: exactly one data node
 *      `reported_state` in {primary, single, wait_primary} (see
 *      `PRIMARY_STATES` below for why `wait_primary` counts), the other in
 *      {secondary}
 *   3. independently confirm the elected primary's container is actually
 *      running via `docker inspect` (a side channel the platform API can't
 *      fake) -- the container name comes straight from cluster-health's
 *      `nodename`, which `PostgresClusterService` deliberately registers
 *      pg_autoctl under so it always matches the Docker container 1:1
 *   4. link the cluster to a project BEFORE deploying (env vars resolve at
 *      deploy-job-creation time), deploy the `db-probe` Go app, and confirm
 *      it actually got the cluster's multi-host `POSTGRES_URL`
 *      (`postgresql://user:pass@host1:port,host2:port/db?target_session_attrs=read-write`)
 *      -- pgx resolves the writable host itself from that DSN, so no
 *      redeploy is needed across a failover
 *   5. write 5 real rows through `/probe`, confirming the app is genuinely
 *      writing to the elected primary
 *   6. `docker stop` the primary's container -- a real, ungraceful-from-the-
 *      cluster's-perspective outage, not an API call. (`docker stop`, not
 *      `kill`: every cluster member's `HostConfig.RestartPolicy` is
 *      `unless-stopped`, so the Docker daemon itself would silently
 *      resurrect a `kill`ed container in place before pg_auto_failover's
 *      monitor ever declares it unhealthy, masking whether real failover
 *      happened at all. `docker stop` is the one signal Docker treats as an
 *      explicit stop and won't auto-restart.)
 *   7. poll cluster-health until a DIFFERENT node reports a writable-primary
 *      state -- proves the monitor actually promoted the surviving replica,
 *      not just that the old primary's row went stale
 *   7.5. confirm the console/CLI-facing API (`GET /external-services/{id}`,
 *      via `get_service_members_with_live_state`) also reflects the
 *      promotion through `ServiceMemberInfo.live_state` -- a different code
 *      path than step 7's raw cluster-health probe. `service_members.role`
 *      itself is intentionally NOT the live signal in the current design
 *      (see README for why); DNS-record republishing was investigated and
 *      skipped as impractical this round -- see README for the reasoning.
 *   8. poll `/probe` (tolerating connection errors while pg_auto_failover
 *      completes the promotion) until writes succeed again, and assert this
 *      happens within a bounded window -- proving the app's existing
 *      connection string routes to the NEW primary with no redeploy,
 *      config change, or app restart
 *   9. assert the post-failover row count is monotonic (> the pre-failover
 *      count) -- the write actually landed, not just that the HTTP call
 *      returned 200
 *  10. teardown (deployment, project, service -- `delete_service` removes
 *      cluster containers by name regardless of running state, so the
 *      stopped ex-primary is cleaned up too) unless `--keep`
 *
 * Real platform bug found and fixed while building this (see
 * `PgAutoFailoverState::is_primary()` in
 * crates/temps-providers/src/externalsvc/cluster_role.rs, and the matching
 * `member_is_live_primary` fix in services.rs): `wait_primary` is a fully
 * writable state (verified live via direct `psql`), not a "not yet
 * writable" transitional one, and for a 2-data-node cluster like this one
 * it's the PERMANENT post-failover steady state, not a brief window --
 * there's no third node to attach as a new standby, so the survivor never
 * progresses past `wait_primary` on its own. `is_primary()` previously
 * excluded it, which broke the DNS reconciler's primary-record republishing
 * and this scenario's own failover detection identically. See the README's
 * "db-ha-failover-scenario steps" section for the full writeup, including a
 * second bug this same fix uncovered in the cluster-member delete-safety
 * gate.
 *
 * Needs a registry the target instance can pull the probe image from --
 * same as `managed-services-scenario`/`backup-restore-scenario`.
 *
 * Uses `buildHaProbeImage` (pgx driver), NOT the shared `buildProbeImage`
 * (lib/pq) every other scenario uses -- see the doc comment on
 * `buildHaProbeImage` in `../lib/probe-app.ts` for why: lib/pq's latest
 * *released* version cannot parse a multi-host `postgresql://` connection
 * string at all (verified live), so it's structurally incapable of
 * exercising this feature, regardless of connection-string formatting.
 */
import { linkServiceToProject, getService, getClusterHealth } from '@temps-sdk/api'
import { makeClient, resolveConfig, unwrap } from '../lib/client.ts'
import {
  createE2eProject,
  createE2eClusterService,
  getProductionEnvironment,
  deployImage,
  waitForDeployment,
  getDeployStatus,
  waitForHttpReady,
  assertNotConsoleFallback,
  resolveLoadTarget,
  teardown,
  makeRunId,
  sleep,
  pollUntil,
  runDockerContainerCommand,
  dockerContainerStatus,
} from '../lib/flows.ts'
import { buildHaProbeImage, PROBE_APP_HEALTH_PATH } from '../lib/probe-app.ts'

export interface DbHaFailoverScenarioOptions {
  registry?: string
  keep?: boolean
  deployTimeout?: string
  clusterTimeout?: string
  failoverTimeout?: string
  json?: boolean
  connection: { url?: string; apiKey?: string }
}

interface StepLog {
  step: string
  ok: boolean
  detail?: string
  ms?: number
}

interface DbHaFailoverScenarioResult {
  runId: string
  ok: boolean
  originalPrimary?: string
  newPrimary?: string
  failoverDetectedMs?: number
  writesRecoveredMs?: number
  finalProbeCount?: number
  steps: StepLog[]
}

async function hitProbe(url: string, headers: Record<string, string>): Promise<number> {
  const res = await fetch(`${url.replace(/\/+$/, '')}/probe`, { headers })
  if (res.status !== 200) throw new Error(`GET /probe returned HTTP ${res.status}`)
  const body = (await res.json()) as { count: number }
  return body.count
}

/**
 * `reported_state` values that mean "this node is currently the writable
 * primary" -- see `PgAutoFailoverState::is_primary`
 * (crates/temps-providers/src/externalsvc/cluster_role.rs).
 *
 * `wait_primary` belongs here: verified live (direct `psql` against a node
 * in this exact state -- `pg_is_in_recovery()` false,
 * `default_transaction_read_only` off, a real `INSERT` succeeding) that
 * pg_auto_failover has already completed promotion by the time it reports
 * `wait_primary`; the state just means "primary with no standby attached
 * yet". For a 2-data-node cluster (this scenario's topology) that's not a
 * brief transition -- it's the stable state the survivor sits in
 * *indefinitely* after failover, because there's no third node available to
 * attach as its replacement standby. Excluding it here previously made
 * every run of this scenario time out waiting for `reported_state` to reach
 * `primary`/`single`, even though the app's writes had already resumed
 * (confirmed live via a direct `/probe` hit against the deployed app during
 * that "stuck" window) -- the platform itself was already treating this
 * exact node as the real primary for actual traffic; only the status
 * classification, here and in `PgAutoFailoverState::is_primary`, was wrong.
 */
const PRIMARY_STATES = new Set(['primary', 'single', 'wait_primary'])

export async function dbHaFailoverScenarioCommand(opts: DbHaFailoverScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps db-ha-failover scenario  ->  ${cfg.url}`)

  const registry = opts.registry ?? process.env.TEMPS_E2E_REGISTRY
  if (!registry) {
    throw new Error(
      'A registry is required: the probe app image must be pushed somewhere the server can pull ' +
        'from. Pass --registry <host:port> (e.g. localhost:5111) or set TEMPS_E2E_REGISTRY.',
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
  const clusterTimeoutMs = Number(opts.clusterTimeout ?? '180000')
  const failoverTimeoutMs = Number(opts.failoverTimeout ?? '90000')
  const scratch = `${process.env.TMPDIR ?? '/tmp'}/temps-e2e-probe`

  const projectIds: number[] = []
  const serviceIds: number[] = []
  const deployments: { projectId: number; deploymentId: number }[] = []
  let originalPrimary: string | undefined
  let newPrimary: string | undefined
  let failoverDetectedMs: number | undefined
  let writesRecoveredMs: number | undefined
  let finalProbeCount: number | undefined

  try {
    const imageRef = await step('build + push db-probe image (pgx driver -- see probe-app.ts for why)', () =>
      buildHaProbeImage({ scratchRoot: scratch, registry, onLog: (l) => log(`    ${l}`) }),
    )
    log(`  image: ${imageRef}`)

    const service = await step('provision a real 1-monitor + 2-data-node postgres HA cluster', () =>
      createE2eClusterService(client, {
        name: `${runId}-ha`,
        serviceType: 'postgres',
        dataNodes: 2,
        parameters: { database: 'app', username: 'app' },
      }),
    )
    serviceIds.push(service.id)
    log(`  service #${service.id} (status: creating, async)`)

    await step('poll service + all 3 members until status="running"', async () => {
      await pollUntil(
        async () => unwrap(await getService({ client, path: { id: service.id } }), 'getService'),
        (d) => {
          if (d.service.status === 'failed') {
            throw new Error(`cluster ${service.id} failed to initialize: ${d.service.error_message}`)
          }
          const members = d.service.members ?? []
          return (
            d.service.status === 'running' &&
            members.length === 3 &&
            members.every((m) => m.status === 'running')
          )
        },
        {
          timeoutMs: clusterTimeoutMs,
          intervalMs: 3000,
          onPoll: (d) =>
            log(
              `    ...service=${d.service.status} members=${(d.service.members ?? [])
                .map((m) => `${m.container_name}:${m.status}`)
                .join(',')}`,
            ),
          label: 'cluster service + all 3 members to reach status=running',
        },
      )
    })

    const steadyState = await step(
      'poll cluster-health until steady state: exactly 1 primary + 1 secondary, both healthy',
      () =>
        pollUntil(
          async () => unwrap(await getClusterHealth({ client, path: { id: service.id } }), 'getClusterHealth'),
          (h) => {
            if (h.monitor_error) return false
            const primaries = h.members.filter((m) => PRIMARY_STATES.has(m.reported_state))
            const secondaries = h.members.filter((m) => m.reported_state === 'secondary')
            return (
              h.members.length === 2 &&
              primaries.length === 1 &&
              secondaries.length === 1 &&
              primaries[0]!.health === 1 &&
              secondaries[0]!.health === 1
            )
          },
          {
            timeoutMs: clusterTimeoutMs,
            intervalMs: 3000,
            onPoll: (h) =>
              log(
                `    ...${h.monitor_error ?? h.members.map((m) => `${m.nodename}=${m.reported_state}(health=${m.health})`).join(', ')}`,
              ),
            label: 'cluster-health to report exactly 1 primary + 1 healthy secondary',
          },
        ),
    )
    originalPrimary = steadyState.members.find((m) => PRIMARY_STATES.has(m.reported_state))!.nodename
    const originalSecondary = steadyState.members.find((m) => m.reported_state === 'secondary')!.nodename
    log(`  primary=${originalPrimary} secondary=${originalSecondary}`)

    await step('independently confirm the elected primary container is actually running (docker inspect)', async () => {
      const status = await dockerContainerStatus(originalPrimary!)
      if (status !== 'running') {
        throw new Error(`docker reports container ${originalPrimary} status="${status}", expected "running"`)
      }
    })

    const project = await step('create project', () =>
      createE2eProject(client, { name: `${runId}-ha-app`, exposedPort: 3000 }),
    )
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    await step('link cluster to project BEFORE deploying (env vars resolve at deploy-job-creation time)', async () => {
      unwrap(
        await linkServiceToProject({ client, path: { id: service.id }, body: { project_id: project.id } }),
        'linkServiceToProject',
      )
    })

    const env = await step('resolve production environment', () => getProductionEnvironment(client, project.id))
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
      // A more generous timeout than the other scenarios' default (30s):
      // this scenario runs a 3-container cluster formation immediately
      // beforehand, so the route-propagation window (async PG NOTIFY, per
      // `assertNotConsoleFallback`'s doc comment) lands on a busier instance
      // than a bare `scenario` run does.
      await assertNotConsoleFallback({ url: target.url, headers: target.headers, timeoutMs: 60_000 })
    })

    await step('health check reports a real DB ping succeeding through the multi-host POSTGRES_URL', async () => {
      const res = await fetch(`${target.url.replace(/\/+$/, '')}${PROBE_APP_HEALTH_PATH}`, {
        headers: target.headers,
      })
      if (res.status !== 200) {
        throw new Error(`GET ${PROBE_APP_HEALTH_PATH} returned HTTP ${res.status} -- multi-host POSTGRES_URL likely missing/unreachable`)
      }
    })

    let preFailoverCount = 0
    await step('write 5 real rows through the injected multi-host POSTGRES_URL', async () => {
      for (let i = 0; i < 5; i++) {
        preFailoverCount = await hitProbe(target.url, target.headers)
      }
      if (preFailoverCount !== 5) throw new Error(`after 5x /probe, count=${preFailoverCount}, expected 5`)
    })
    log(`  pre-failover count=${preFailoverCount}`)

    await step(`docker stop the primary container (${originalPrimary}) -- simulates a real outage`, async () => {
      const res = await runDockerContainerCommand('stop', originalPrimary!)
      if (res.code !== 0) {
        throw new Error(`docker stop ${originalPrimary} failed (exit ${res.code}): ${res.stderr.trim()}`)
      }
      const status = await dockerContainerStatus(originalPrimary!)
      if (status === 'running') {
        throw new Error(`docker stop ${originalPrimary} returned 0 but container still reports status="running"`)
      }
    })

    const failoverStart = performance.now()
    const postFailoverHealth = await step(
      `poll cluster-health until a DIFFERENT node is promoted primary (bounded ${Math.round(failoverTimeoutMs / 1000)}s)`,
      () =>
        pollUntil(
          async () => unwrap(await getClusterHealth({ client, path: { id: service.id } }), 'getClusterHealth'),
          (h) => {
            const primary = h.members.find((m) => PRIMARY_STATES.has(m.reported_state) && m.health === 1)
            return !!primary && primary.nodename !== originalPrimary
          },
          {
            timeoutMs: failoverTimeoutMs,
            intervalMs: 2000,
            onPoll: (h) =>
              log(
                `    ...${h.monitor_error ?? h.members.map((m) => `${m.nodename}=${m.reported_state}(health=${m.health})`).join(', ')}`,
              ),
            label: `a node other than ${originalPrimary} to be promoted primary`,
          },
        ),
    )
    failoverDetectedMs = performance.now() - failoverStart
    newPrimary = postFailoverHealth.members.find((m) => PRIMARY_STATES.has(m.reported_state) && m.health === 1)!.nodename
    if (newPrimary !== originalSecondary) {
      throw new Error(
        `expected the surviving replica ${originalSecondary} to be promoted, but the new primary is ${newPrimary}`,
      )
    }
    log(`  new primary=${newPrimary} (promoted in ${(failoverDetectedMs / 1000).toFixed(1)}s)`)

    await step('independently confirm the NEW primary container is actually running (docker inspect)', async () => {
      const status = await dockerContainerStatus(newPrimary!)
      if (status !== 'running') {
        throw new Error(`docker reports new primary container ${newPrimary} status="${status}", expected "running"`)
      }
    })

    // Platform-level symptom check #1 from the PR's own root-cause
    // narrative: does the CONSOLE-FACING API also reflect the promotion,
    // not just the raw cluster-health probe already checked above? This
    // exercises a genuinely different code path --
    // `get_service_members_with_live_state` (called from `get_service_info`
    // / `GET /external-services/{id}`), which is what the console's role
    // badge and the CLI actually read. Note: `service_members.role` itself
    // is NOT the live signal in the current design (it's intentionally
    // config-only -- `monitor`/`replica` -- per `get_service_info`'s doc
    // comment: "The UI uses `live_state` for the role badge ... instead of
    // being gated on the `service_members.role` reconciler"; confirmed by
    // reading the code that nothing ever writes `role="primary"` at
    // runtime), so `live_state` is the correct, current equivalent of the
    // "role failing to flip" symptom the PR's commit message describes.
    await step(
      'confirm the console-facing API (GET /external-services/{id}) reflects the promotion via live_state',
      async () => {
        const d = unwrap(await getService({ client, path: { id: service.id } }), 'getService')
        const members = d.service.members ?? []
        const promoted = members.find((m) => m.container_name === newPrimary)
        if (!promoted) {
          throw new Error(`getService did not return a member for the promoted node ${newPrimary}`)
        }
        if (!promoted.live_state || !PRIMARY_STATES.has(promoted.live_state)) {
          throw new Error(
            `promoted node ${newPrimary}'s ServiceMemberInfo.live_state="${promoted.live_state}" is not a ` +
              `primary state -- the console/CLI-facing API (get_service_members_with_live_state) has not ` +
              `caught up with the failover the raw cluster-health probe already detected`,
          )
        }
        // NOT asserted: that the OLD primary's live_state stops reading a
        // primary state. Verified live that it doesn't -- `reported_state`
        // is the monitor's last-heard-from value for that node
        // (`ClusterMemberHealth.reported_state`'s own doc comment: "Doesn't
        // change when the node stops phoning home"), and a `docker stop`ped
        // node can't un-report itself. The monitor has nothing further to
        // do with a dead node's OWN row once it has promoted the survivor,
        // so it can legitimately sit at a stale `primary` indefinitely.
        // `ServiceMemberInfo` (unlike `ClusterMemberHealth`) carries no
        // `health` field to disambiguate "stale dead primary" from "a
        // second live primary" -- a real console-facing gap, but a
        // separate one from this PR's fix, and out of scope here.
        const demoted = members.find((m) => m.container_name === originalPrimary)
        log(`  console-facing API: ${newPrimary}=${promoted.live_state}, ${originalPrimary}=${demoted?.live_state ?? 'n/a'} (may be stale -- see comment above)`)
      },
    )

    const recoveryStart = performance.now()
    let postFailoverCount = 0
    await step(
      `poll /probe (through the SAME app, SAME connection string, no redeploy) until writes succeed again (bounded ${Math.round(failoverTimeoutMs / 1000)}s)`,
      async () => {
        const deadline = recoveryStart + failoverTimeoutMs
        let lastErr: Error | undefined
        while (performance.now() < deadline) {
          try {
            postFailoverCount = await hitProbe(target.url, target.headers)
            writesRecoveredMs = performance.now() - recoveryStart
            log(`    ...write recovered, count=${postFailoverCount} (${(writesRecoveredMs / 1000).toFixed(1)}s after stop)`)
            return
          } catch (e) {
            lastErr = e as Error
            log(`    ...write still failing: ${lastErr.message}`)
            await sleep(2000)
          }
        }
        throw new Error(
          `writes did not recover within ${Math.round(failoverTimeoutMs / 1000)}s of stopping the primary ` +
            `(last error: ${lastErr?.message})`,
        )
      },
    )

    await step('post-failover row count is monotonic (the write actually landed on the new primary)', async () => {
      if (postFailoverCount <= preFailoverCount) {
        throw new Error(
          `post-failover /probe count=${postFailoverCount}, expected > pre-failover count=${preFailoverCount}`,
        )
      }
      finalProbeCount = postFailoverCount
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
  const result: DbHaFailoverScenarioResult = {
    runId,
    ok,
    originalPrimary,
    newPrimary,
    failoverDetectedMs,
    writesRecoveredMs,
    finalProbeCount,
    steps,
  }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ db-ha-failover-scenario PASSED' : '❌ db-ha-failover-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
