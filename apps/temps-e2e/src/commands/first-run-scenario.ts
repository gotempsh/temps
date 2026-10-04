// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * First-run suite: everything a brand-new operator tries in their first hour,
 * against a freshly installed instance, each step timed to its first
 * successful response through the proxy.
 *
 *   docker-image      Docker-image project from a public image
 *   git-dockerfile    public git URL, built from the directory's Dockerfile
 *   node-preset       Node app uploaded as source; preset detected by Temps
 *   compose           docker-compose project uploaded as source
 *   static-site       static bundle upload
 *   managed-services  managed Postgres + Redis linked to the Node app; a
 *                     redeploy must deliver POSTGRES_URL / REDIS_URL to the
 *                     container and both must accept connections from it
 *
 * Steps are independent: a failure is recorded with the deployment's
 * classified failure and the failing job's last log lines, and the suite
 * continues with the next step. `managed-services` reuses the Node project
 * from `node-preset` when that step succeeded, otherwise deploys its own.
 *
 * The sample apps live in examples/first-run/. `git-dockerfile` clones them
 * from a public https URL (`--git-url`/`--git-branch`); Temps refuses
 * file://, git:// and private-address remotes (validate_git_url), so a local
 * git server is not an option.
 */
import { existsSync } from 'node:fs'
import { writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { createProject, getLastDeployment, linkServiceToProject } from '@temps-sdk/api'
import type { Client } from '@temps-sdk/api/client'
import { makeClient, resolveConfig, unwrap, type TempsClientConfig } from '../lib/client.ts'
import {
  createE2eGitProject,
  createE2eProject,
  createE2eService,
  deployImage,
  getProductionEnvironment,
  makeRunId,
  pollUntil,
  teardown,
} from '../lib/flows.ts'
import {
  awaitDeployment,
  awaitServedBody,
  awaitServiceRunning,
  DeploymentFailedError,
  deploySourceArchive,
  deployStaticArchive,
  inspectArchive,
  zipDirectory,
  type DeploymentFailure,
  type DropCandidate,
} from '../lib/first-run.ts'

export const FIRST_RUN_STEPS = [
  'docker-image',
  'git-dockerfile',
  'node-preset',
  'compose',
  'static-site',
  'managed-services',
] as const
export type FirstRunStepKey = (typeof FIRST_RUN_STEPS)[number]

export interface FirstRunScenarioOptions {
  only?: string[]
  skip?: string[]
  image?: string
  imagePort?: string
  gitUrl?: string
  gitBranch?: string
  gitDirectory?: string
  examplesDir?: string
  deployTimeout?: string
  buildTimeout?: string
  report?: string
  keep?: boolean
  json?: boolean
  connection: { url?: string; apiKey?: string }
}

export interface FirstRunPhase {
  name: string
  ms: number
}

export interface FirstRunStepResult {
  key: FirstRunStepKey
  title: string
  ok: boolean
  skipped?: boolean
  /** From the step's first API call to the first correct response through the proxy. */
  timeToSuccessMs?: number
  phases: FirstRunPhase[]
  detail?: string
  error?: string
  failure?: DeploymentFailure
}

export interface FirstRunReport {
  runId: string
  url: string
  ok: boolean
  startedAt: string
  finishedAt: string
  steps: FirstRunStepResult[]
}

const TITLES: Record<FirstRunStepKey, string> = {
  'docker-image': 'Docker image (public registry)',
  'git-dockerfile': 'Git repository with a Dockerfile',
  'node-preset': 'Node app via preset (source upload)',
  compose: 'docker-compose project (source upload)',
  'static-site': 'Static site (bundle upload)',
  'managed-services': 'Managed Postgres + Redis linked, env reaches app',
}

interface Context {
  client: Client
  cfg: TempsClientConfig
  runId: string
  examplesDir: string
  deployTimeoutMs: number
  buildTimeoutMs: number
  log: (msg: string) => void
  resources: {
    deployments: { projectId: number; deploymentId: number }[]
    projectIds: number[]
    serviceIds: number[]
  }
  /** The Node project from `node-preset`, reused by `managed-services`. */
  nodeApp?: { projectId: number; environmentId: number; mainUrl: string; archive: Uint8Array<ArrayBuffer> }
}

/** Times named phases and keeps them on the step result. */
class StepTimer {
  readonly phases: FirstRunPhase[] = []
  readonly started = performance.now()
  constructor(private readonly log: (msg: string) => void) {}

  async phase<T>(name: string, fn: () => Promise<T>): Promise<T> {
    const t0 = performance.now()
    this.log(`    - ${name}`)
    try {
      return await fn()
    } finally {
      this.phases.push({ name, ms: Math.round(performance.now() - t0) })
    }
  }

  elapsed(): number {
    return Math.round(performance.now() - this.started)
  }
}

function track(ctx: Context, projectId: number, deploymentId: number): void {
  ctx.resources.deployments.push({ projectId, deploymentId })
}

function parseJson(body: string): Record<string, unknown> | undefined {
  try {
    const parsed = JSON.parse(body)
    return parsed && typeof parsed === 'object' ? (parsed as Record<string, unknown>) : undefined
  } catch {
    return undefined
  }
}

function pickCandidate(candidates: DropCandidate[], predicate: (c: DropCandidate) => boolean, what: string): DropCandidate {
  const candidate = candidates.find(predicate)
  if (!candidate) {
    throw new Error(
      `preset detection did not offer ${what}; candidates were ${JSON.stringify(candidates.map((c) => `${c.preset}${c.isStatic ? ' (static)' : ''} @ ${c.directory}`))}`,
    )
  }
  return candidate
}

async function createUploadProject(ctx: Context, name: string, candidate: DropCandidate) {
  const created = unwrap(
    await createProject({
      client: ctx.client,
      body: {
        name,
        directory: candidate.directory,
        main_branch: 'main',
        preset: candidate.preset,
        source_type: candidate.isStatic ? 'static_files' : 'uploaded_source',
        automatic_deploy: false,
        storage_service_ids: [],
        preset_config:
          candidate.preset === 'docker-compose'
            ? {
                composePath: candidate.composePath ?? 'compose.yaml',
                // Compose is private until a public route is explicitly selected.
                publicPorts: [{ service: 'web', port: 80 }],
              }
            : undefined,
      },
    }),
    `createProject(${name})`,
  )
  ctx.resources.projectIds.push(created.id)
  return created
}

// ── steps ───────────────────────────────────────────────────────────────────

async function stepDockerImage(ctx: Context, t: StepTimer, image: string, port: number): Promise<string> {
  const project = await t.phase('create Docker-image project', () =>
    createE2eProject(ctx.client, { name: `${ctx.runId}-image`, exposedPort: port }),
  )
  ctx.resources.projectIds.push(project.id)
  const env = await t.phase('resolve environment', () => getProductionEnvironment(ctx.client, project.id))
  const deploymentId = await t.phase('deploy image', () =>
    deployImage(ctx.client, { projectId: project.id, environmentId: env.id, imageRef: image }),
  )
  track(ctx, project.id, deploymentId)
  await t.phase('wait for deployment', () =>
    awaitDeployment(ctx.client, { projectId: project.id, deploymentId, timeoutMs: ctx.deployTimeoutMs }),
  )
  await t.phase('first response through proxy', () =>
    awaitServedBody({
      instanceUrl: ctx.cfg.url,
      appUrl: env.mainUrl,
      timeoutMs: 120_000,
      description: `image ${image}`,
      accept: (status) => status >= 200 && status < 400,
    }),
  )
  return `${image} -> ${env.mainUrl}`
}

async function stepGitDockerfile(
  ctx: Context,
  t: StepTimer,
  git: { url: string; branch: string; directory: string },
): Promise<string> {
  const parsed = new URL(git.url)
  const [owner, repo] = parsed.pathname.replace(/^\/+/, '').replace(/\.git$/, '').split('/')
  if (!owner || !repo) throw new Error(`--git-url ${git.url} is not an https://host/owner/repo URL`)
  const project = await t.phase('create git project (public URL)', () =>
    createE2eGitProject(ctx.client, {
      name: `${ctx.runId}-git`,
      repoOwner: owner,
      repoName: repo,
      gitUrl: git.url,
      directory: git.directory,
      preset: 'dockerfile',
      mainBranch: git.branch,
    }),
  )
  ctx.resources.projectIds.push(project.id)
  const env = await t.phase('resolve environment', () => getProductionEnvironment(ctx.client, project.id))
  // Creating a git project queues its first deployment by itself; that is
  // the deployment a new user waits on, so it is the one timed here.
  const deployment = await t.phase('initial deployment is queued', () =>
    pollUntil(
      async () => (await getLastDeployment({ client: ctx.client, path: { id: project.id } })).data ?? null,
      (d) => d !== null,
      { timeoutMs: 60_000, intervalMs: 1000, label: `initial deployment of git project ${project.id}` },
    ),
  )
  track(ctx, project.id, deployment!.id)
  await t.phase('clone + docker build + deploy', () =>
    awaitDeployment(ctx.client, { projectId: project.id, deploymentId: deployment!.id, timeoutMs: ctx.buildTimeoutMs }),
  )
  await t.phase('first response through proxy', () =>
    awaitServedBody({
      instanceUrl: ctx.cfg.url,
      appUrl: env.mainUrl,
      timeoutMs: 120_000,
      description: 'git Dockerfile app',
      accept: (status, body) => status === 200 && parseJson(body)?.app === 'first-run-dockerfile',
    }),
  )
  return `${git.url}#${git.branch}:${git.directory}`
}

async function stepNodePreset(ctx: Context, t: StepTimer): Promise<string> {
  const archive = await t.phase('zip examples/first-run/node-app', () =>
    zipDirectory(resolve(ctx.examplesDir, 'node-app')),
  )
  const candidates = await t.phase('detect preset', () => inspectArchive(ctx.client, archive, 'node-app.zip'))
  const candidate = pickCandidate(
    candidates,
    (c) => !c.isStatic && c.preset !== 'docker-compose' && c.preset !== 'dockerfile',
    'a buildable Node preset',
  )
  const project = await t.phase(`create project (preset ${candidate.preset})`, () =>
    createUploadProject(ctx, `${ctx.runId}-node`, candidate),
  )
  const env = await t.phase('resolve environment', () => getProductionEnvironment(ctx.client, project.id))
  const deploymentId = await t.phase('upload source', () =>
    deploySourceArchive(ctx.client, { projectId: project.id, environmentId: env.id, data: archive }),
  )
  track(ctx, project.id, deploymentId)
  await t.phase('build + deploy', () =>
    awaitDeployment(ctx.client, { projectId: project.id, deploymentId, timeoutMs: ctx.buildTimeoutMs }),
  )
  await t.phase('first response through proxy', () =>
    awaitServedBody({
      instanceUrl: ctx.cfg.url,
      appUrl: env.mainUrl,
      timeoutMs: 120_000,
      description: 'Node preset app',
      accept: (status, body) => status === 200 && parseJson(body)?.app === 'first-run-node',
    }),
  )
  ctx.nodeApp = { projectId: project.id, environmentId: env.id, mainUrl: env.mainUrl, archive }
  return `preset ${candidate.preset} (${candidate.label})`
}

async function stepCompose(ctx: Context, t: StepTimer): Promise<string> {
  const archive = await t.phase('zip examples/first-run/compose-app', () =>
    zipDirectory(resolve(ctx.examplesDir, 'compose-app')),
  )
  const candidates = await t.phase('detect preset', () => inspectArchive(ctx.client, archive, 'compose-app.zip'))
  const candidate = pickCandidate(candidates, (c) => c.preset === 'docker-compose', 'the docker-compose preset')
  const project = await t.phase('create compose project', () =>
    createUploadProject(ctx, `${ctx.runId}-compose`, candidate),
  )
  const env = await t.phase('resolve environment', () => getProductionEnvironment(ctx.client, project.id))
  const deploymentId = await t.phase('upload source', () =>
    deploySourceArchive(ctx.client, { projectId: project.id, environmentId: env.id, data: archive }),
  )
  track(ctx, project.id, deploymentId)
  await t.phase('pull + start stack', () =>
    awaitDeployment(ctx.client, { projectId: project.id, deploymentId, timeoutMs: ctx.deployTimeoutMs }),
  )
  await t.phase('first response through proxy', () =>
    awaitServedBody({
      instanceUrl: ctx.cfg.url,
      appUrl: env.mainUrl,
      timeoutMs: 120_000,
      description: 'compose web service',
      accept: (status, body) => status === 200 && body.includes('Name: first-run-compose'),
    }),
  )
  return `compose file ${candidate.composePath ?? 'compose.yaml'}`
}

async function stepStaticSite(ctx: Context, t: StepTimer): Promise<string> {
  const archive = await t.phase('zip examples/first-run/static-site', () =>
    zipDirectory(resolve(ctx.examplesDir, 'static-site')),
  )
  const candidates = await t.phase('detect preset', () => inspectArchive(ctx.client, archive, 'static-site.zip'))
  const candidate = pickCandidate(candidates, (c) => c.isStatic, 'a static-site candidate')
  const project = await t.phase('create static project', () =>
    createUploadProject(ctx, `${ctx.runId}-static`, candidate),
  )
  const env = await t.phase('resolve environment', () => getProductionEnvironment(ctx.client, project.id))
  const deploymentId = await t.phase('upload bundle + deploy', () =>
    deployStaticArchive(ctx.client, { projectId: project.id, environmentId: env.id, data: archive }),
  )
  track(ctx, project.id, deploymentId)
  await t.phase('wait for deployment', () =>
    awaitDeployment(ctx.client, { projectId: project.id, deploymentId, timeoutMs: ctx.deployTimeoutMs }),
  )
  await t.phase('first response through proxy', () =>
    awaitServedBody({
      instanceUrl: ctx.cfg.url,
      appUrl: env.mainUrl,
      timeoutMs: 120_000,
      description: 'static site',
      accept: (status, body) => status === 200 && body.includes('first-run-static-site'),
    }),
  )
  return `preset ${candidate.preset}`
}

interface ServiceProbe {
  present: boolean
  parsed: boolean
  reachable: boolean
}

async function readEnvReport(ctx: Context, mainUrl: string, accept: (r: Record<string, ServiceProbe>) => boolean, description: string, timeoutMs: number) {
  const body = await awaitServedBody({
    instanceUrl: ctx.cfg.url,
    appUrl: mainUrl,
    path: '/env',
    timeoutMs,
    description,
    accept: (status, text) => {
      if (status !== 200) return false
      const report = parseJson(text) as Record<string, ServiceProbe> | undefined
      return !!report && accept(report)
    },
  })
  return parseJson(body) as Record<string, ServiceProbe>
}

async function stepManagedServices(ctx: Context, t: StepTimer): Promise<string> {
  let app = ctx.nodeApp
  if (!app) {
    ctx.log('    (node-preset did not leave an app behind; deploying a fresh one)')
    await stepNodePreset(ctx, t)
    app = ctx.nodeApp
    if (!app) throw new Error('could not deploy the Node app that managed services link to')
  }
  const target = app

  await t.phase('baseline: app has no service variables', () =>
    readEnvReport(
      ctx,
      target.mainUrl,
      (r) => r.POSTGRES_URL?.present === false && r.REDIS_URL?.present === false,
      'baseline /env',
      60_000,
    ),
  )

  const postgres = await t.phase('create managed Postgres', () =>
    createE2eService(ctx.client, {
      name: `${ctx.runId}-pg`,
      serviceType: 'postgres',
      parameters: { database: 'app', username: 'app' },
    }),
  )
  ctx.resources.serviceIds.push(postgres.id)
  const redis = await t.phase('create managed Redis', () =>
    createE2eService(ctx.client, { name: `${ctx.runId}-redis`, serviceType: 'redis', parameters: {} }),
  )
  ctx.resources.serviceIds.push(redis.id)
  await t.phase('services report running', async () => {
    await awaitServiceRunning(ctx.client, postgres.id, ctx.deployTimeoutMs)
    await awaitServiceRunning(ctx.client, redis.id, ctx.deployTimeoutMs)
  })
  await t.phase('link both services to the app', async () => {
    for (const service of [postgres, redis]) {
      unwrap(
        await linkServiceToProject({ client: ctx.client, path: { id: service.id }, body: { project_id: target.projectId } }),
        `linkServiceToProject(service ${service.id} -> project ${target.projectId})`,
      )
    }
  })
  // Variables are resolved when a deployment is planned, so linking only
  // takes effect on the next deployment: redeploy the same source.
  const deploymentId = await t.phase('redeploy', () =>
    deploySourceArchive(ctx.client, { projectId: target.projectId, environmentId: target.environmentId, data: target.archive }),
  )
  track(ctx, target.projectId, deploymentId)
  await t.phase('wait for redeploy', () =>
    awaitDeployment(ctx.client, { projectId: target.projectId, deploymentId, timeoutMs: ctx.buildTimeoutMs }),
  )
  const report = await t.phase('POSTGRES_URL and REDIS_URL reach the app and connect', () =>
    readEnvReport(
      ctx,
      target.mainUrl,
      (r) => !!r.POSTGRES_URL?.reachable && !!r.REDIS_URL?.reachable,
      'linked-service /env',
      180_000,
    ),
  )
  return `postgres #${postgres.id}, redis #${redis.id}: ${JSON.stringify(report)}`
}

// ── runner ──────────────────────────────────────────────────────────────────

export function selectSteps(only?: string[], skip?: string[]): FirstRunStepKey[] {
  const known = new Set<string>(FIRST_RUN_STEPS)
  for (const key of [...(only ?? []), ...(skip ?? [])]) {
    if (!known.has(key)) {
      throw new Error(`unknown first-run step "${key}"; expected one of ${FIRST_RUN_STEPS.join(', ')}`)
    }
  }
  return FIRST_RUN_STEPS.filter(
    (key) => (!only || only.length === 0 || only.includes(key)) && !(skip ?? []).includes(key),
  )
}

/** Render the per-step summary as a Markdown table (GitHub job summary). */
export function renderMarkdownSummary(report: FirstRunReport): string {
  const seconds = (ms?: number) => (ms === undefined ? '-' : `${(ms / 1000).toFixed(1)}s`)
  const rows = report.steps.map((step) => {
    const status = step.skipped ? 'skipped' : step.ok ? 'pass' : 'FAIL'
    const note = step.ok ? (step.detail ?? '') : (step.failure ? `[${step.failure.classification}] ` : '') + (step.error ?? '')
    return `| ${step.title} | ${status} | ${seconds(step.timeToSuccessMs)} | ${note.replace(/\|/g, '\\|').slice(0, 300)} |`
  })
  const failures = report.steps
    .filter((step) => step.failure)
    .map((step) => {
      const f = step.failure!
      return [
        `### ${step.title}: deployment ${f.deploymentId} (${f.status}, ${f.classification})`,
        f.failedJob ? `Job \`${f.failedJob.name}\` (${f.failedJob.jobId}) ended \`${f.failedJob.status}\`.` : '',
        '```text',
        ...f.logTail,
        '```',
      ].join('\n')
    })
  return [
    `## First-run suite: ${report.ok ? 'passed' : 'FAILED'}`,
    '',
    '| Step | Result | Time to success | Notes |',
    '| --- | --- | --- | --- |',
    ...rows,
    '',
    ...failures,
  ].join('\n')
}

export async function firstRunScenarioCommand(opts: FirstRunScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  const examplesDir = resolve(opts.examplesDir ?? resolve(import.meta.dir, '../../../../examples/first-run'))
  if (!existsSync(resolve(examplesDir, 'node-app/package.json'))) {
    throw new Error(`examples directory ${examplesDir} does not contain the first-run sample apps`)
  }
  const ctx: Context = {
    client,
    cfg,
    runId: makeRunId(Date.now()),
    examplesDir,
    deployTimeoutMs: Number(opts.deployTimeout ?? '300000'),
    buildTimeoutMs: Number(opts.buildTimeout ?? '900000'),
    log,
    resources: { deployments: [], projectIds: [], serviceIds: [] },
  }
  const selected = selectSteps(opts.only, opts.skip)
  const startedAt = new Date().toISOString()
  log(`Temps first-run suite  ->  ${cfg.url}  (run ${ctx.runId})`)

  const results: FirstRunStepResult[] = []
  for (const key of FIRST_RUN_STEPS) {
    if (!selected.includes(key)) {
      results.push({ key, title: TITLES[key], ok: true, skipped: true, phases: [] })
      continue
    }
    log(`\n▶ ${TITLES[key]}`)
    const t = new StepTimer(log)
    const result: FirstRunStepResult = { key, title: TITLES[key], ok: false, phases: t.phases }
    try {
      switch (key) {
        case 'docker-image':
          result.detail = await stepDockerImage(ctx, t, opts.image ?? 'traefik/whoami:v1.10', Number(opts.imagePort ?? '80'))
          break
        case 'git-dockerfile':
          result.detail = await stepGitDockerfile(ctx, t, {
            url: opts.gitUrl ?? 'https://github.com/gotempsh/temps.git',
            branch: opts.gitBranch ?? 'main',
            directory: opts.gitDirectory ?? 'examples/first-run/dockerfile-app',
          })
          break
        case 'node-preset':
          result.detail = await stepNodePreset(ctx, t)
          break
        case 'compose':
          result.detail = await stepCompose(ctx, t)
          break
        case 'static-site':
          result.detail = await stepStaticSite(ctx, t)
          break
        case 'managed-services':
          result.detail = await stepManagedServices(ctx, t)
          break
      }
      result.ok = true
      result.timeToSuccessMs = t.elapsed()
      log(`  ✓ ${TITLES[key]} in ${(result.timeToSuccessMs / 1000).toFixed(1)}s`)
    } catch (error) {
      result.error = (error as Error).message
      if (error instanceof DeploymentFailedError) result.failure = error.failure
      log(`  ✗ ${TITLES[key]}: ${result.error}`)
      for (const line of result.failure?.logTail ?? []) log(`      | ${line}`)
    }
    results.push(result)
  }

  if (opts.keep) {
    log(`\n(kept: projects=${ctx.resources.projectIds.join(',')} services=${ctx.resources.serviceIds.join(',')})`)
  } else {
    const td = await teardown(client, ctx.resources)
    log(
      `\n▶ teardown: ${td.teardownDeployments} deployment(s), ${td.deletedProjects} project(s), ` +
        `${td.deletedServices} service(s)` + (td.errors.length ? `, ${td.errors.length} error(s)` : ''),
    )
    for (const e of td.errors) log(`    ! ${e}`)
  }

  const report: FirstRunReport = {
    runId: ctx.runId,
    url: cfg.url,
    ok: results.every((r) => r.ok),
    startedAt,
    finishedAt: new Date().toISOString(),
    steps: results,
  }
  if (opts.report) {
    await writeFile(opts.report, JSON.stringify(report, null, 2) + '\n')
    await writeFile(opts.report.replace(/\.json$/, '') + '.md', renderMarkdownSummary(report) + '\n')
  }
  if (json) {
    process.stdout.write(JSON.stringify(report, null, 2) + '\n')
  } else {
    log('\n' + renderMarkdownSummary(report))
  }
  if (!report.ok) process.exitCode = 1
}
