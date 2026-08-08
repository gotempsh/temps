/**
 * Real git-triggered deploy against a live Temps instance -- proves the
 * actual git pipeline (clone a public repo, build a subdirectory with a
 * language preset, deploy, serve), not just an image pull or a locally
 * synthesized Dockerfile the way `scenario`/`examples` do:
 *   1. create a project pointed at a real public repo
 *      (github.com/gotempsh/temps-examples), scoped to one subdirectory
 *      (`examples/starters/go/net-http`) via `directory`, with
 *      `automatic_deploy: false` so exactly one deployment happens
 *   2. resolve its production environment
 *   3. `POST /projects/{id}/trigger-pipeline` -- the same action a real git
 *      push to the tracked branch would cause -- and poll
 *      `GET /projects/{id}/last-deployment` for the deployment it created
 *      (`TriggerPipelineResponse` doesn't carry a deployment id itself)
 *   4. wait for the deployment to go healthy -- real clone + real build, so
 *      this needs a materially longer timeout than the prebuilt-image
 *      scenarios
 *   5. hit the deployed app's `/` and `/health` and assert their EXACT JSON
 *      bodies match the checked-in source (`{"message":"Hello from Go on
 *      Temps!"}` / `{"status":"ok"}`) -- proving the real upstream repo got
 *      cloned and built, not a cached/stale/wrong artifact
 *   6. teardown (deployment, project)
 *
 * Deliberately hits real github.com -- a documented exception to the
 * "no real internet" rule the rest of this suite holds itself to (see
 * backup-restore-scenario/tls-scenario/email-scenario, which all point at
 * local test infra instead). There's no local substitute for "does Temps's
 * git-clone-and-build pipeline work against a real repo host" that would
 * actually prove this.
 */
import { getDeployStatus, waitForDeployment } from '../lib/flows.ts'
import {
  createE2eGitProject,
  getProductionEnvironment,
  triggerPipelineAndGetDeploymentId,
  resolveLoadTarget,
  waitForHttpReady,
  assertNotConsoleFallback,
  teardown,
  makeRunId,
} from '../lib/flows.ts'
import { makeClient, resolveConfig } from '../lib/client.ts'

export interface GitDeployScenarioOptions {
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

interface GitDeployScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
}

const REPO_OWNER = 'gotempsh'
const REPO_NAME = 'temps-examples'
const REPO_DIRECTORY = 'examples/starters/go/net-http'

export async function gitDeployScenarioCommand(opts: GitDeployScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps git-deploy scenario  ->  ${cfg.url}`)

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

  // Real clone + real build (not a prebuilt-image pull), so this needs more
  // headroom than the other scenarios' 300000ms default.
  const deployTimeoutMs = Number(opts.deployTimeout ?? '600000')

  const projectIds: number[] = []
  const deployments: { projectId: number; deploymentId: number }[] = []

  try {
    const project = await step('create a project pointed at a real public repo', () =>
      createE2eGitProject(client, {
        name: `${runId}-git-app`,
        repoOwner: REPO_OWNER,
        repoName: REPO_NAME,
        gitUrl: `https://github.com/${REPO_OWNER}/${REPO_NAME}.git`,
        directory: REPO_DIRECTORY,
        preset: 'go',
      }),
    )
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    const env = await step('resolve production environment', () => getProductionEnvironment(client, project.id))

    const deploymentId = await step('trigger the real git pipeline (clone + build)', () =>
      triggerPipelineAndGetDeploymentId(client, { projectId: project.id, environmentId: env.id, branch: 'main' }),
    )
    deployments.push({ projectId: project.id, deploymentId })
    log(`  deployment #${deploymentId}`)

    await step('wait for the build+deploy to go healthy', () =>
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
    await step('wait for the app to serve real traffic', async () => {
      await waitForHttpReady({ url: target.url, headers: target.headers })
      await assertNotConsoleFallback({ url: target.url, headers: target.headers })
    })

    await step('/ returns the exact body baked into the real repo source', async () => {
      const res = await fetch(target.url, { headers: target.headers })
      if (res.status !== 200) throw new Error(`GET / returned HTTP ${res.status}`)
      const body = await res.json()
      const expected = { message: 'Hello from Go on Temps!' }
      if (JSON.stringify(body) !== JSON.stringify(expected)) {
        throw new Error(`GET / returned ${JSON.stringify(body)}, expected ${JSON.stringify(expected)}`)
      }
    })

    await step('/health returns the exact body baked into the real repo source', async () => {
      const res = await fetch(`${target.url.replace(/\/+$/, '')}/health`, { headers: target.headers })
      if (res.status !== 200) throw new Error(`GET /health returned HTTP ${res.status}`)
      const body = await res.json()
      const expected = { status: 'ok' }
      if (JSON.stringify(body) !== JSON.stringify(expected)) {
        throw new Error(`GET /health returned ${JSON.stringify(body)}, expected ${JSON.stringify(expected)}`)
      }
    })
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(`\n(kept resources: projects=${projectIds.join(',')})`)
    } else {
      const td = await teardown(client, { deployments, projectIds })
      log(
        `\n▶ teardown: tore down ${td.teardownDeployments} deployment(s), deleted ${td.deletedProjects} project(s)` +
          (td.errors.length ? ` (${td.errors.length} errors)` : ''),
      )
      for (const e of td.errors) log(`    ! ${e}`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: GitDeployScenarioResult = { runId, ok, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ git-deploy-scenario PASSED' : '❌ git-deploy-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
