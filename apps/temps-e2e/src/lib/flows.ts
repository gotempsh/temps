/**
 * High-level Temps control-plane flows used by the e2e commands: create a
 * project, provision a database service, deploy a prebuilt image, poll for
 * health, resolve the public URL, and verify traffic via proxy logs.
 *
 * Each helper takes the per-call `client` so a run is fully isolated.
 */
import {
  createProject,
  deleteProject,
  getEnvironments,
  deployFromImage,
  getDeployment,
  createService,
  deleteService,
  getProxyLogs,
  teardownDeployment,
} from '@temps-sdk/api'
import type { Client } from '@temps-sdk/api/client'
import { unwrap } from './client.ts'

/** Terminal-success deployment states (see ADR / generated DeploymentResponse.state). */
const DEPLOY_SUCCESS = new Set(['completed', 'succeeded', 'running', 'active'])
const DEPLOY_FAILED = new Set(['failed', 'cancelled', 'errored', 'error'])

export interface CreatedProject {
  id: number
  name: string
  slug: string
}

/** Create a Dockerfile/image-based project tagged with the run id. */
export async function createE2eProject(
  client: Client,
  opts: { name: string; exposedPort?: number },
): Promise<CreatedProject> {
  const res = await createProject({
    client,
    body: {
      name: opts.name,
      directory: '.',
      main_branch: 'main',
      // 'dockerfile' preset is the generic container path; image deploys don't
      // need a git repo or build command.
      preset: 'dockerfile',
      storage_service_ids: [],
      source_type: 'docker_image',
      exposed_port: opts.exposedPort ?? 80,
      is_web_app: true,
    },
  })
  const p = unwrap(res, 'createProject')
  return { id: p.id, name: p.name, slug: p.slug }
}

/** Pick the production (non-preview) environment for a project. */
export async function getProductionEnvironment(
  client: Client,
  projectId: number,
): Promise<{ id: number; mainUrl: string; name: string }> {
  const res = await getEnvironments({ client, path: { project_id: projectId } })
  const envs = unwrap(res, 'getEnvironments')
  const prod = envs.find((e) => e.is_preview === false) ?? envs[0]
  if (!prod) throw new Error(`No environment found for project ${projectId}`)
  return { id: prod.id, mainUrl: prod.main_url, name: prod.name }
}

/** Trigger a deploy from a prebuilt public image; returns the deployment id. */
export async function deployImage(
  client: Client,
  opts: { projectId: number; environmentId: number; imageRef: string },
): Promise<number> {
  const res = await deployFromImage({
    client,
    path: { project_id: opts.projectId, environment_id: opts.environmentId },
    body: { image_ref: opts.imageRef },
  })
  const d = unwrap(res, 'deployFromImage')
  return d.id
}

export interface DeployStatus {
  state: string
  url?: string | null
  terminal: boolean
  ok: boolean
}

/** Read a deployment's current state. */
export async function getDeployStatus(
  client: Client,
  projectId: number,
  deploymentId: number,
): Promise<DeployStatus> {
  const res = await getDeployment({
    client,
    path: { project_id: projectId, deployment_id: deploymentId },
  })
  const d = unwrap(res, 'getDeployment') as { state?: string; status?: string; url?: string | null }
  const state = (d.state ?? d.status ?? 'unknown').toLowerCase()
  const ok = DEPLOY_SUCCESS.has(state)
  const failed = DEPLOY_FAILED.has(state)
  return { state, url: d.url, terminal: ok || failed, ok }
}

/** Poll a deployment until it reaches a terminal state or times out. */
export async function waitForDeployment(
  client: Client,
  opts: {
    projectId: number
    deploymentId: number
    timeoutMs?: number
    intervalMs?: number
    onPoll?: (s: DeployStatus, elapsedMs: number) => void
  },
): Promise<DeployStatus> {
  const timeoutMs = opts.timeoutMs ?? 5 * 60_000
  const intervalMs = opts.intervalMs ?? 3000
  const start = performance.now()
  // eslint-disable-next-line no-constant-condition
  while (true) {
    const s = await getDeployStatus(client, opts.projectId, opts.deploymentId)
    const elapsed = performance.now() - start
    opts.onPoll?.(s, elapsed)
    if (s.terminal) return s
    if (elapsed > timeoutMs) {
      throw new Error(
        `Deployment ${opts.deploymentId} did not reach a terminal state within ${Math.round(timeoutMs / 1000)}s (last state: ${s.state})`,
      )
    }
    await sleep(intervalMs)
  }
}

export interface CreatedService {
  id: number
  name: string
}

/** Provision an external service (postgres/redis/etc.) tagged with the run id. */
export async function createE2eService(
  client: Client,
  opts: {
    name: string
    serviceType: 'postgres' | 'redis' | 'mongodb' | 's3' | 'kv' | 'blob' | 'rustfs' | 'minio'
    version?: string
    parameters?: Record<string, unknown>
  },
): Promise<CreatedService> {
  const res = await createService({
    client,
    body: {
      name: opts.name,
      service_type: opts.serviceType,
      parameters: opts.parameters ?? {},
      ...(opts.version ? { version: opts.version } : {}),
    },
  })
  const s = unwrap(res, 'createService') as { id: number; name: string }
  return { id: s.id, name: s.name }
}

/**
 * Count proxy-log entries recorded for a host since a given time. Used to verify
 * that generated traffic actually landed on the proxy/ingest pipeline.
 */
export async function countProxyLogs(
  client: Client,
  opts: { host?: string; sinceMs?: number },
): Promise<number> {
  const query: Record<string, unknown> = { page: 1, page_size: 1 }
  if (opts.host) query.host = opts.host
  if (opts.sinceMs) query.start_date = new Date(opts.sinceMs).toISOString()
  const res = await getProxyLogs({ client, query })
  const data = unwrap(res, 'getProxyLogs') as { total?: number; logs?: unknown[] }
  return data.total ?? data.logs?.length ?? 0
}

/**
 * Best-effort teardown of resources created by a run. Never throws.
 * Order matters: tear down running deployments (stops the container + removes
 * the proxy route) BEFORE deleting the project row, otherwise the container can
 * linger serving traffic after the project is gone.
 */
export async function teardown(
  client: Client,
  resources: {
    deployments?: { projectId: number; deploymentId: number }[]
    projectIds?: number[]
    serviceIds?: number[]
  },
): Promise<{
  teardownDeployments: number
  deletedProjects: number
  deletedServices: number
  errors: string[]
}> {
  const errors: string[] = []
  let teardownDeployments = 0
  let deletedProjects = 0
  let deletedServices = 0

  for (const d of resources.deployments ?? []) {
    try {
      await teardownDeployment({
        client,
        path: { project_id: d.projectId, deployment_id: d.deploymentId },
      })
      teardownDeployments++
    } catch (e) {
      errors.push(`teardownDeployment(${d.deploymentId}): ${(e as Error).message}`)
    }
  }
  // teardownDeployment returns before the container is fully removed (the stop
  // is async server-side). Give it a short grace so deleting the project below
  // doesn't orphan a still-stopping container.
  if ((resources.deployments ?? []).length > 0) {
    await sleep(3000)
  }
  for (const id of resources.projectIds ?? []) {
    try {
      await deleteProject({ client, path: { id } })
      deletedProjects++
    } catch (e) {
      errors.push(`deleteProject(${id}): ${(e as Error).message}`)
    }
  }
  for (const id of resources.serviceIds ?? []) {
    try {
      await deleteService({ client, path: { id } })
      deletedServices++
    } catch (e) {
      errors.push(`deleteService(${id}): ${(e as Error).message}`)
    }
  }
  return { teardownDeployments, deletedProjects, deletedServices, errors }
}

export function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms))
}

/**
 * Turn a deployment's public `main_url` into a target the load generator can
 * actually reach. The proxy routes by Host header, so we send requests to the
 * INSTANCE's address (e.g. http://localhost:8080) while passing the app's
 * hostname as the Host header. This avoids depending on the app's scheme/port
 * (the main_url is often https on :443) and on external DNS — it always lands on
 * the same proxy that serves real traffic.
 */
export function resolveLoadTarget(
  instanceUrl: string,
  appMainUrl: string,
): { url: string; host: string; logHost: string; headers: Record<string, string> } {
  const parsed = new URL(appMainUrl)
  // `host` includes the port (e.g. `app.sslip.io:8080`) — this is what the proxy
  // routes on, so it's the Host header we send.
  const appHost = parsed.host
  // The proxy records the bare HOSTNAME (no port) in `proxy_logs.host`, so the
  // verification query must match on hostname-only — otherwise a host carrying a
  // non-standard port (the common local case) never matches and traffic looks
  // "unrecorded" even though it landed.
  const logHost = parsed.hostname
  // Strip any /api suffix the instance URL might carry; the proxy data plane is
  // the bare origin.
  const base = instanceUrl.replace(/\/+$/, '').replace(/\/api$/, '')
  const baseUrl = new URL(base)
  // Hit the proxy origin with the app's host header.
  const url = `${baseUrl.protocol}//${baseUrl.host}/`
  return { url, host: appHost, logHost, headers: { Host: appHost } }
}

/**
 * Poll an HTTP target until it returns a non-5xx/non-0 response or times out.
 * Confirms the deployed app is actually serving before we load-test it (the
 * deployment status string alone is not a reliability signal).
 */
export async function waitForHttpReady(opts: {
  url: string
  headers?: Record<string, string>
  timeoutMs?: number
  intervalMs?: number
  onPoll?: (status: number, elapsedMs: number) => void
}): Promise<number> {
  const timeoutMs = opts.timeoutMs ?? 120_000
  const intervalMs = opts.intervalMs ?? 2000
  const start = performance.now()
  let last = 0
  while (performance.now() - start < timeoutMs) {
    try {
      const ctrl = new AbortController()
      const t = setTimeout(() => ctrl.abort(), 10_000)
      const res = await fetch(opts.url, { headers: opts.headers, signal: ctrl.signal })
      clearTimeout(t)
      await res.arrayBuffer().catch(() => undefined)
      last = res.status
      opts.onPoll?.(res.status, performance.now() - start)
      if (res.status > 0 && res.status < 500) return res.status
    } catch {
      opts.onPoll?.(0, performance.now() - start)
    }
    await sleep(intervalMs)
  }
  throw new Error(
    `App did not become HTTP-ready within ${Math.round(timeoutMs / 1000)}s (last status: ${last})`,
  )
}

/**
 * True when a response body is the Temps console SPA shell, not an app.
 *
 * Must match ONLY the console — a deployed React/Vite app is also an HTML
 * document with a `<div id="root">`, so matching on that generic marker gives
 * false positives. Key on the console's own page <title> (and its favicon path),
 * which a deployed example never sets.
 */
function looksLikeConsoleFallback(body: string): boolean {
  if (!/<!doctype html>/i.test(body)) return false
  // The Temps console index.html sets `<title>Temps</title>`. A static SPA
  // example sets its own title (e.g. "react-basic"), so this stays specific.
  return /<title>\s*Temps\s*<\/title>/i.test(body)
}

/**
 * Poll the target until it serves the deployed app rather than the Temps console
 * SPA fallback, then return. The proxy serves the console shell (HTTP 200,
 * `<title>Temps</title>`) for unknown/failed hosts AND for the brief window
 * before a freshly-started container's route propagates — so a bare status check
 * cannot distinguish "app is serving" from "deploy failed / not routed yet".
 * This is the guard that makes a broken deploy actually fail (after giving the
 * route a fair chance to propagate).
 */
export async function assertNotConsoleFallback(opts: {
  url: string
  headers?: Record<string, string>
  timeoutMs?: number
  intervalMs?: number
}): Promise<void> {
  const timeoutMs = opts.timeoutMs ?? 30_000
  const intervalMs = opts.intervalMs ?? 1500
  const start = performance.now()
  let body = ''
  let status = 0
  while (performance.now() - start < timeoutMs) {
    const ctrl = new AbortController()
    const t = setTimeout(() => ctrl.abort(), 10_000)
    try {
      const res = await fetch(opts.url, { headers: opts.headers, signal: ctrl.signal })
      status = res.status
      body = await res.text()
      if (!looksLikeConsoleFallback(body)) return // real app response
    } catch {
      // transient — retry
    } finally {
      clearTimeout(t)
    }
    await sleep(intervalMs)
  }
  throw new Error(
    `served the Temps console fallback (HTTP ${status}) instead of the app after ` +
      `${Math.round(timeoutMs / 1000)}s — the deployment is not actually serving ` +
      `(body starts: ${JSON.stringify(body.slice(0, 80))})`,
  )
}

/** A stable, unique-ish run id derived from a timestamp passed in by the caller. */
export function makeRunId(nowMs: number): string {
  return `e2e-${nowMs.toString(36)}`
}
