// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Shared helpers for the first-run suite (`first-run-scenario`) and the
 * quiet-logs soak fixture (`quiet-logs-fixture`).
 *
 * The point of both is to behave like a brand-new operator, so these helpers
 * use the same public endpoints the console and `temps deploy drop` use, and
 * when a deployment fails they explain why in the terms an operator would
 * need: the failure class, the job that failed and the last lines it logged.
 */
import { spawn } from 'node:child_process'
import { mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  deployFromStatic,
  deployFromUploadedSource,
  getDeployment,
  getDeploymentJobLogs,
  getDeploymentJobs,
  getService,
  inspectDropArchive,
  uploadStaticBundle,
} from '@temps-sdk/api'
import type { Client } from '@temps-sdk/api/client'
import { unwrap } from './client.ts'
import {
  isTerminalDeployStatus,
  looksLikeConsoleFallback,
  resolveLoadTarget,
  sleep,
} from './flows.ts'

/** Number of job-log lines attached to a failure report. */
export const FAILURE_LOG_TAIL_LINES = 40

/**
 * Failure classes, most specific first. Mirrors the server's
 * `classify_failure_reason` (crates/temps-deployments/src/services/
 * workflow_execution_service.rs) so a nightly failure is labelled with the
 * same vocabulary the deploy-failed telemetry uses.
 */
const FAILURE_CLASSES: { label: string; needles: string[] }[] = [
  { label: 'out_of_memory', needles: ['out of memory', 'oomkilled', 'exit code 137'] },
  { label: 'disk_exhausted', needles: ['no space left on device', 'disk quota exceeded', 'enospc'] },
  { label: 'health_check', needles: ['health check', 'healthcheck', 'unhealthy'] },
  { label: 'timeout', needles: ['timeout', 'timed out', 'deadline'] },
  {
    label: 'repository_authentication',
    needles: ['authentication failed', 'could not read username', 'permission denied (publickey)', 'invalid credentials'],
  },
  { label: 'image_missing', needles: ['manifest unknown', 'pull access denied', 'image not found', 'no such image'] },
  { label: 'build_error', needles: ['build', 'compile', 'nixpacks', 'dockerfile'] },
  { label: 'network', needles: ['clone', 'network', 'connection', 'download', 'dns'] },
  { label: 'cancelled', needles: ['cancel'] },
]

export function classifyFailure(reason: string | null | undefined): string {
  if (!reason) return 'unknown'
  const lower = reason.toLowerCase()
  for (const entry of FAILURE_CLASSES) {
    if (entry.needles.some((needle) => lower.includes(needle))) return entry.label
  }
  return 'unknown'
}

/** Keep the last `count` non-empty lines of a job log. */
export function tailLines(text: string, count: number): string[] {
  const lines = text.split(/\r?\n/).filter((line) => line.trim().length > 0)
  return lines.slice(Math.max(0, lines.length - count))
}

export interface DeploymentFailure {
  deploymentId: number
  status: string
  classification: string
  reason: string | null
  failedJob: { jobId: string; name: string; status: string } | null
  logTail: string[]
}

/** Raised when a deployment ends badly; carries everything needed to triage it. */
export class DeploymentFailedError extends Error {
  constructor(readonly failure: DeploymentFailure) {
    super(
      `deployment ${failure.deploymentId} ended "${failure.status}" ` +
        `[${failure.classification}]` +
        (failure.failedJob ? ` in job "${failure.failedJob.name}" (${failure.failedJob.jobId})` : '') +
        (failure.reason ? `: ${failure.reason}` : ''),
    )
    this.name = 'DeploymentFailedError'
  }
}

/** Gather the classified failure and last job-log lines for a deployment. */
export async function explainDeployment(
  client: Client,
  projectId: number,
  deploymentId: number,
  status: string,
): Promise<DeploymentFailure> {
  let reason: string | null = null
  let failedJob: DeploymentFailure['failedJob'] = null
  let logTail: string[] = []
  try {
    const deployment = unwrap(
      await getDeployment({ client, path: { project_id: projectId, deployment_id: deploymentId } }),
      `getDeployment(${deploymentId})`,
    )
    reason = deployment.cancelled_reason ?? null
  } catch (error) {
    reason = `could not read deployment ${deploymentId}: ${(error as Error).message}`
  }
  try {
    const jobs = unwrap(
      await getDeploymentJobs({ client, path: { project_id: projectId, deployment_id: deploymentId } }),
      `getDeploymentJobs(${deploymentId})`,
    ).jobs
    // The job that failed explains the failure; when nothing is marked
    // failed (a timeout), the last job that started is where it got stuck.
    const ordered = [...jobs].sort((a, b) => (a.execution_order ?? 0) - (b.execution_order ?? 0))
    const culprit =
      ordered.find((job) => job.status.toLowerCase() === 'failure' || job.status.toLowerCase() === 'failed') ??
      [...ordered].reverse().find((job) => job.started_at != null) ??
      null
    if (culprit) {
      failedJob = { jobId: culprit.job_id, name: culprit.name, status: culprit.status }
      reason = culprit.error_message ?? reason
      const logs = await getDeploymentJobLogs({
        client,
        path: { project_id: projectId, deployment_id: deploymentId, job_id: culprit.job_id },
        parseAs: 'text',
      })
      if (typeof logs.data === 'string') logTail = tailLines(logs.data, FAILURE_LOG_TAIL_LINES)
    }
  } catch (error) {
    logTail = [`(could not read job logs: ${(error as Error).message})`]
  }
  return {
    deploymentId,
    status,
    classification: classifyFailure(reason ?? status),
    reason,
    failedJob,
    logTail,
  }
}

/**
 * Poll a deployment to a terminal state. A failed, cancelled or timed-out
 * deployment raises `DeploymentFailedError` with the classified failure and
 * the failing job's log tail, never just "it failed".
 */
export async function awaitDeployment(
  client: Client,
  opts: { projectId: number; deploymentId: number; timeoutMs: number; onPoll?: (state: string) => void },
): Promise<void> {
  const start = performance.now()
  let state = 'unknown'
  while (performance.now() - start < opts.timeoutMs) {
    const res = await getDeployment({
      client,
      path: { project_id: opts.projectId, deployment_id: opts.deploymentId },
    })
    const deployment = unwrap(res, `getDeployment(${opts.deploymentId})`)
    state = deployment.status.toLowerCase()
    opts.onPoll?.(state)
    const { terminal, ok } = isTerminalDeployStatus(state)
    if (terminal && ok) return
    if (terminal) {
      throw new DeploymentFailedError(
        await explainDeployment(client, opts.projectId, opts.deploymentId, state),
      )
    }
    await sleep(3000)
  }
  const failure = await explainDeployment(client, opts.projectId, opts.deploymentId, `${state} (timed out)`)
  failure.classification = 'timeout'
  failure.reason = `still "${state}" after ${Math.round(opts.timeoutMs / 1000)}s${failure.reason ? `; ${failure.reason}` : ''}`
  throw new DeploymentFailedError(failure)
}

/**
 * Poll the proxy (with the app's Host header) until `accept(body)` holds.
 * The console SPA fallback never counts as the app answering.
 */
export async function awaitServedBody(opts: {
  instanceUrl: string
  appUrl: string
  path?: string
  timeoutMs: number
  accept: (status: number, body: string) => boolean
  description: string
}): Promise<string> {
  const target = resolveLoadTarget(opts.instanceUrl, opts.appUrl)
  const url = target.url.replace(/\/+$/, '') + (opts.path ?? '/')
  const start = performance.now()
  let lastStatus = 0
  let lastBody = ''
  while (performance.now() - start < opts.timeoutMs) {
    const controller = new AbortController()
    const timer = setTimeout(() => controller.abort(), 10_000)
    try {
      const res = await fetch(url, { headers: target.headers, signal: controller.signal })
      lastStatus = res.status
      lastBody = await res.text()
      if (!looksLikeConsoleFallback(lastBody) && opts.accept(lastStatus, lastBody)) return lastBody
    } catch (error) {
      lastStatus = 0
      lastBody = (error as Error).message
    } finally {
      clearTimeout(timer)
    }
    await sleep(2000)
  }
  const fallback = looksLikeConsoleFallback(lastBody) ? ' (the Temps console fallback: the route never reached the app)' : ''
  throw new Error(
    `${opts.description}: ${url} with Host ${target.host} did not serve the expected response within ` +
      `${Math.round(opts.timeoutMs / 1000)}s; last HTTP ${lastStatus}${fallback}, body starts ` +
      JSON.stringify(lastBody.slice(0, 160)),
  )
}

/** Zip a directory with the system `zip`, the same way `temps deploy drop` does. */
export async function zipDirectory(directory: string): Promise<Uint8Array<ArrayBuffer>> {
  const scratch = await mkdtemp(join(tmpdir(), 'temps-first-run-'))
  const archive = join(scratch, 'source.zip')
  try {
    await new Promise<void>((resolvePromise, reject) => {
      const child = spawn('zip', ['-q', '-r', '-X', archive, '.', '-x', '.DS_Store', '*/.DS_Store'], {
        cwd: directory,
        stdio: ['ignore', 'ignore', 'pipe'],
      })
      let stderr = ''
      child.stderr.on('data', (chunk) => (stderr += String(chunk)))
      child.once('error', (error) => reject(new Error(`zip could not start for ${directory}: ${error.message}`)))
      child.once('exit', (code) =>
        code === 0 ? resolvePromise() : reject(new Error(`zip exited ${code} for ${directory}: ${stderr.trim()}`)),
      )
    })
    return new Uint8Array(await readFile(archive))
  } finally {
    await rm(scratch, { recursive: true, force: true })
  }
}

function zipFile(data: Uint8Array<ArrayBuffer>, name: string): File {
  return new File([data], name, { type: 'application/zip' })
}

export interface DropCandidate {
  preset: string
  directory: string
  isStatic: boolean
  composePath?: string | null
  label: string
}

/** Ask the server which preset it detects for an archive (the `drop` path). */
export async function inspectArchive(client: Client, data: Uint8Array<ArrayBuffer>, name: string): Promise<DropCandidate[]> {
  const inspected = unwrap(await inspectDropArchive({ client, body: { file: zipFile(data, name) } }), 'inspectDropArchive')
  return inspected.candidates.map((candidate) => ({
    preset: candidate.preset,
    directory: candidate.directory,
    isStatic: candidate.isStatic,
    composePath: candidate.composePath,
    label: candidate.label,
  }))
}

/** Upload a source archive and start a preset build; returns the deployment id. */
export async function deploySourceArchive(
  client: Client,
  opts: { projectId: number; environmentId: number; data: Uint8Array<ArrayBuffer> },
): Promise<number> {
  const deployment = unwrap(
    await deployFromUploadedSource({
      client,
      path: { project_id: opts.projectId, environment_id: opts.environmentId },
      body: { file: zipFile(opts.data, 'source.zip') },
    }),
    `deployFromUploadedSource(project ${opts.projectId})`,
  )
  return deployment.id
}

/** Upload a static bundle and deploy it; returns the deployment id. */
export async function deployStaticArchive(
  client: Client,
  opts: { projectId: number; environmentId: number; data: Uint8Array<ArrayBuffer> },
): Promise<number> {
  const bundle = unwrap(
    await uploadStaticBundle({
      client,
      path: { project_id: opts.projectId },
      body: { file: zipFile(opts.data, 'site.zip') },
    }),
    `uploadStaticBundle(project ${opts.projectId})`,
  )
  const deployment = unwrap(
    await deployFromStatic({
      client,
      path: { project_id: opts.projectId, environment_id: opts.environmentId },
      body: { static_bundle_id: bundle.id },
    }),
    `deployFromStatic(project ${opts.projectId}, bundle ${bundle.id})`,
  )
  return deployment.id
}

/** Wait until a managed service reports `running`. */
export async function awaitServiceRunning(client: Client, serviceId: number, timeoutMs: number): Promise<void> {
  const start = performance.now()
  let status = 'unknown'
  while (performance.now() - start < timeoutMs) {
    const detail = unwrap(await getService({ client, path: { id: serviceId } }), `getService(${serviceId})`)
    status = detail.service.status
    if (status === 'running') return
    if (status === 'failed' || status === 'error') {
      throw new Error(`managed service ${serviceId} reported status "${status}"`)
    }
    await sleep(3000)
  }
  throw new Error(`managed service ${serviceId} still "${status}" after ${Math.round(timeoutMs / 1000)}s`)
}
