// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Builds (or tears down) the steady-state workload for the quiet-logs soak
 * (scripts/first-run/quiet-logs-soak.sh): the smallest instance that still
 * exercises every background loop an idle operator's server runs.
 *
 *   - one deployed app (public image), so route-table, container-health and
 *     log-collection loops have something to watch
 *   - one managed Postgres linked to it, so service health and backup
 *     schedulers run
 *   - one error alert rule on the project, so alert evaluation runs
 *   - one uptime monitor on the app's environment, so the status-page
 *     checker probes it on its interval
 *
 * `--state <file>` records the created ids; `--teardown --state <file>`
 * deletes exactly those resources, so the soak never touches anything else.
 */
import { readFile, writeFile } from 'node:fs/promises'
import { createAlertRule, createMonitor, deleteMonitor, linkServiceToProject } from '@temps-sdk/api'
import { makeClient, resolveConfig, unwrap } from '../lib/client.ts'
import {
  createE2eProject,
  createE2eService,
  deployImage,
  getProductionEnvironment,
  makeRunId,
  teardown,
} from '../lib/flows.ts'
import { awaitDeployment, awaitServedBody, awaitServiceRunning } from '../lib/first-run.ts'

export interface QuietLogsFixtureOptions {
  state: string
  teardown?: boolean
  verify?: boolean
  image?: string
  imagePort?: string
  deployTimeout?: string
  json?: boolean
  connection: { url?: string; apiKey?: string }
}

export interface QuietLogsFixtureState {
  runId: string
  projectId: number
  environmentId: number
  deploymentId: number
  serviceId: number
  alertRuleId: number
  monitorId: number
  appUrl: string
  createdAt: string
}

export async function quietLogsFixtureCommand(opts: QuietLogsFixtureOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const log = (msg: string) => {
    if (!opts.json) process.stderr.write(msg + '\n')
  }

  if (opts.verify) {
    const state = JSON.parse(await readFile(opts.state, 'utf8')) as QuietLogsFixtureState
    if (!state.projectId || !state.deploymentId || !state.serviceId || !state.appUrl) {
      throw new Error('Soak fixture state is incomplete; cannot verify workload health')
    }
    await awaitServiceRunning(client, state.serviceId, 15_000)
    await awaitDeployment(client, {
      projectId: state.projectId, deploymentId: state.deploymentId, timeoutMs: 15_000,
    })
    await awaitServedBody({
      instanceUrl: cfg.url, appUrl: state.appUrl, timeoutMs: 15_000,
      description: 'soak workload after the idle window',
      accept: (status) => status >= 200 && status < 400,
    })
    log('soak workload still healthy')
    return
  }

  if (opts.teardown) {
    const state = JSON.parse(await readFile(opts.state, 'utf8')) as Partial<QuietLogsFixtureState>
    const errors: string[] = []
    if (state.monitorId) {
      const res = await deleteMonitor({ client, path: { monitor_id: state.monitorId } })
      if (res.error) errors.push(`deleteMonitor(${state.monitorId}): HTTP ${res.response?.status}`)
    }
    const td = await teardown(client, {
      deployments: state.projectId && state.deploymentId ? [{ projectId: state.projectId, deploymentId: state.deploymentId }] : [],
      projectIds: state.projectId ? [state.projectId] : [],
      serviceIds: state.serviceId ? [state.serviceId] : [],
    })
    errors.push(...td.errors)
    log(`quiet-logs fixture removed (${errors.length} error(s))`)
    for (const e of errors) log(`  ! ${e}`)
    if (errors.length) process.exitCode = 1
    return
  }

  const runId = makeRunId(Date.now())
  const state: Partial<QuietLogsFixtureState> = { runId, createdAt: new Date().toISOString() }
  // Persist after every step so a half-built fixture can still be torn down.
  const save = () => writeFile(opts.state, JSON.stringify(state, null, 2) + '\n')
  const deployTimeoutMs = Number(opts.deployTimeout ?? '300000')
  const image = opts.image ?? 'traefik/whoami:v1.10'

  try {
    const project = await createE2eProject(client, { name: `${runId}-soak`, exposedPort: Number(opts.imagePort ?? '80') })
    state.projectId = project.id
    await save()
    log(`project #${project.id}`)

    const service = await createE2eService(client, {
      name: `${runId}-soak-pg`,
      serviceType: 'postgres',
      parameters: { database: 'app', username: 'app' },
    })
    state.serviceId = service.id
    await save()
    await awaitServiceRunning(client, service.id, deployTimeoutMs)
    unwrap(
      await linkServiceToProject({ client, path: { id: service.id }, body: { project_id: project.id } }),
      `linkServiceToProject(service ${service.id} -> project ${project.id})`,
    )
    log(`postgres #${service.id} linked`)

    const env = await getProductionEnvironment(client, project.id)
    state.environmentId = env.id
    state.appUrl = env.mainUrl
    const deploymentId = await deployImage(client, { projectId: project.id, environmentId: env.id, imageRef: image })
    state.deploymentId = deploymentId
    await save()
    await awaitDeployment(client, { projectId: project.id, deploymentId, timeoutMs: deployTimeoutMs })
    await awaitServedBody({
      instanceUrl: cfg.url,
      appUrl: env.mainUrl,
      timeoutMs: 120_000,
      description: `soak app ${image}`,
      accept: (status) => status >= 200 && status < 400,
    })
    log(`deployment #${deploymentId} serving ${env.mainUrl}`)

    const rule = unwrap(
      await createAlertRule({
        client,
        path: { project_id: project.id },
        body: { name: `${runId}-new-issue`, trigger_type: 'new_issue', enabled: true },
      }),
      `createAlertRule(project ${project.id})`,
    )
    state.alertRuleId = rule.id
    await save()
    log(`alert rule #${rule.id}`)

    const monitor = unwrap(
      await createMonitor({
        client,
        path: { project_id: project.id },
        body: { name: `${runId}-uptime`, monitor_type: 'http', environment_id: env.id, check_interval_seconds: 60 },
      }),
      `createMonitor(project ${project.id})`,
    )
    state.monitorId = monitor.id
    await save()
    log(`monitor #${monitor.id}`)
  } catch (error) {
    await save()
    throw error
  }

  if (opts.json) process.stdout.write(JSON.stringify(state, null, 2) + '\n')
}
