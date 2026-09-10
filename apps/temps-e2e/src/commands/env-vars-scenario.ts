// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Project/app environment-variable management against a live Temps
 * instance -- proving the full lifecycle (create / update / delete / scope
 * to a specific environment) actually reaches a real running container, not
 * just that the API round-trips a row.
 *
 * The one thing that makes this worth testing end to end: there is NO
 * hot-reload. `crates/temps-environments/src/services/env_var_service.rs`
 * only ever writes the `env_vars` / `env_var_environments` tables -- a
 * running container keeps whatever environment it was started with until
 * the next deploy re-resolves and re-injects env vars at container-start
 * time (`temps-deployments/src/services/env_resolver.rs`). A scenario that
 * only asserts "the API returned the value I just wrote" would pass even if
 * that injection path were completely broken. This scenario instead:
 *
 *   1. deploys the repo's `examples/echo-server` image, which responds to
 *      every request with a JSON body that includes `env: process.env` --
 *      i.e. the ACTUAL environment the container process sees, not
 *      anything the platform claims it set.
 *   2. creates a project env var with a distinct marker value, scoped to
 *      production only, and asserts the CREATE response returns the
 *      plaintext value (round-trip).
 *   3. redeploys (a fresh `deployFromImage` call against the same image --
 *      the only way env var changes ever take effect) and polls the live
 *      container's own JSON response until `env["<key>"]` actually equals
 *      the marker value -- proving propagation into the real process, not a
 *      DB row.
 *   4. updates the var to a NEW marker value and redeploys again, asserting
 *      the live container now reflects the new value -- proves updates
 *      propagate on redeploy, not just creates.
 *   5. deletes the var and redeploys once more, asserting the key is now
 *      absent from the container's environment entirely (not just masked or
 *      empty-stringed).
 *   6. a lighter-weight, deploy-free check of environment scoping: a var
 *      created with `environment_ids: [production]` must NOT appear when
 *      `GET .../env-vars` is filtered by a second (staging) environment's
 *      id, proving `get_environment_variables`'s environment_id filter
 *      (crates/temps-environments/src/services/env_var_service.rs ~line
 *      140) actually excludes unscoped vars rather than just annotating
 *      them.
 *
 * The list endpoint (`GET /projects/{id}/env-vars`) always masks non-secret
 * values with `"***"` (handler.rs `get_environment_variables`, deliberately
 * -- see its comment about not bulk-dumping plaintext over a single GET), so
 * plaintext round-trip assertions here use the CREATE/UPDATE responses
 * instead, which do return `var.value` verbatim.
 */
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import {
  createEnvironment,
  createEnvironmentVariable,
  updateEnvironmentVariable,
  deleteEnvironmentVariable,
  getEnvironmentVariables,
} from '@temps-sdk/api'
import { makeClient, resolveConfig, unwrap } from '../lib/client.ts'
import {
  createE2eProject,
  getProductionEnvironment,
  deployImage,
  waitForDeployment,
  getDeployStatus,
  waitForHttpReady,
  assertNotConsoleFallback,
  fetchBody,
  resolveLoadTarget,
  teardown,
  makeRunId,
  sleep,
} from '../lib/flows.ts'

export interface EnvVarsScenarioOptions {
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

interface EnvVarsScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
}

/** The container port `examples/echo-server` listens on ($PORT, defaults to 8080). */
const ECHO_SERVER_PORT = 8080

/** Locate the repo's `examples/echo-server` dir relative to this app (apps/temps-e2e). */
function echoServerSourceDir(): string {
  const here = dirname(fileURLToPath(import.meta.url)) // .../apps/temps-e2e/src/commands
  return resolve(here, '../../../../examples/echo-server')
}

/**
 * `docker build` (+ push) the repo's existing `examples/echo-server` image
 * as-is -- it already ships its own Dockerfile and has zero npm
 * dependencies (see its package.json), so unlike `versioned-app.ts`/
 * `probe-app.ts` there is no scratch-context rendering step: this builds
 * directly against the committed source tree (read-only; docker build never
 * mutates the context) and reuses the platform's own example gallery entry
 * instead of inventing a parallel test-only fixture.
 */
async function buildEchoServerImage(opts: {
  registry: string
  tag?: string
  onLog?: (line: string) => void
}): Promise<string> {
  const srcDir = echoServerSourceDir()
  const imageRef = opts.tag ?? `${opts.registry}/e2e-echo-server:latest`

  opts.onLog?.(`docker build -t ${imageRef} ${srcDir}`)
  await runDocker(['build', '--load', '-t', imageRef, srcDir], opts.onLog, `docker build ${imageRef}`)
  opts.onLog?.(`docker push ${imageRef}`)
  await runDocker(['push', imageRef], opts.onLog, `docker push ${imageRef}`)
  return imageRef
}

/** Spawn a docker subcommand, stream its output through onLog, throw on failure. */
async function runDocker(
  args: string[],
  onLog: ((line: string) => void) | undefined,
  what: string,
): Promise<void> {
  const proc = Bun.spawn(['docker', ...args], { stdout: 'pipe', stderr: 'pipe' })
  const pump = async (stream: ReadableStream<Uint8Array>) => {
    const reader = stream.getReader()
    const decoder = new TextDecoder()
    let buf = ''
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      buf += decoder.decode(value, { stream: true })
      const lines = buf.split('\n')
      buf = lines.pop() ?? ''
      for (const l of lines) if (l.trim()) onLog?.(l)
    }
    if (buf.trim()) onLog?.(buf)
  }
  await Promise.all([pump(proc.stdout), pump(proc.stderr)])
  const code = await proc.exited
  if (code !== 0) {
    throw new Error(`${what} failed (exit ${code})`)
  }
}

/** Shape of `examples/echo-server`'s JSON response body (only the fields this scenario reads). */
interface EchoServerBody {
  env?: Record<string, string>
}

/**
 * Poll a live `echo-server` target until its OWN reported `env["<key>"]`
 * matches `expected` (or, when `expected` is `undefined`, until the key is
 * genuinely absent), or time out. Unlike a plain HTTP-ready check, this
 * reads the value back out of the deployed container's real process
 * environment on every poll -- proving propagation, not just that a new
 * deployment reached a terminal state.
 */
async function waitForEnvValue(
  target: { url: string; headers?: Record<string, string> },
  key: string,
  expected: string | undefined,
  opts: { timeoutMs?: number; intervalMs?: number; label: string },
): Promise<void> {
  const timeoutMs = opts.timeoutMs ?? 90_000
  const intervalMs = opts.intervalMs ?? 2000
  const start = performance.now()
  let lastValue: string | undefined
  let lastStatus = 0
  while (performance.now() - start < timeoutMs) {
    try {
      const { status, body } = await fetchBody(target.url, target.headers, 10_000)
      lastStatus = status
      if (status >= 200 && status < 300) {
        const parsed = JSON.parse(body) as EchoServerBody
        lastValue = parsed.env?.[key]
        if (lastValue === expected) return
      }
    } catch {
      // transient (connection reset / non-JSON body during route flap) -- retry
    }
    await sleep(intervalMs)
  }
  throw new Error(
    `${opts.label}: expected env["${key}"] to be ${JSON.stringify(expected)}, ` +
      `last observed ${JSON.stringify(lastValue)} (HTTP ${lastStatus}) after ${Math.round(timeoutMs / 1000)}s`,
  )
}

export async function envVarsScenarioCommand(opts: EnvVarsScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps env-vars scenario  ->  ${cfg.url}`)

  const registry = opts.registry ?? process.env.TEMPS_E2E_REGISTRY
  if (!registry) {
    throw new Error(
      'A registry is required: the echo-server image must be pushed somewhere the server can ' +
        'pull from. Pass --registry <host:port> (e.g. localhost:5111) or set TEMPS_E2E_REGISTRY. ' +
        'Start a local one with: docker run -d -p 5111:5000 --name temps-e2e-registry registry:2',
    )
  }

  const runId = makeRunId(Date.now())
  const deployTimeoutMs = Number(opts.deployTimeout ?? '300000')
  const markerKey = 'E2E_ENV_VAR_MARKER'
  const valueA = `temps-e2e-env-var-A-${runId}`
  const valueB = `temps-e2e-env-var-B-${runId}`
  const scopeKey = 'E2E_ENV_VAR_SCOPE_TEST'
  const scopeValue = `temps-e2e-scope-${runId}`

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
  const envVarIds: { projectId: number; varId: number }[] = []
  let stagingEnvId: number | undefined

  try {
    const project = await step('create project', () =>
      createE2eProject(client, { name: `${runId}-envvars`, exposedPort: ECHO_SERVER_PORT }),
    )
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    const env = await step('resolve production environment', () =>
      getProductionEnvironment(client, project.id),
    )
    log(`  env #${env.id} (${env.name})  url=${env.mainUrl}`)
    const target = resolveLoadTarget(cfg.url, env.mainUrl)

    const image = await step('build + push echo-server image', () =>
      buildEchoServerImage({ registry, onLog: (l) => log(`    ${l}`) }),
    )
    log(`  image: ${image}`)

    // --- 1. initial deploy, no marker set yet ---
    const deploymentId1 = await step('deploy echo-server', () =>
      deployImage(client, { projectId: project.id, environmentId: env.id, imageRef: image }),
    )
    deployments.push({ projectId: project.id, deploymentId: deploymentId1 })
    log(`  deployment #${deploymentId1}`)

    await step('wait for initial deployment', () =>
      waitForDeployment(client, {
        projectId: project.id,
        deploymentId: deploymentId1,
        timeoutMs: deployTimeoutMs,
        onPoll: (s) => log(`    ...${s.state}`),
      }),
    )
    await step('confirm initial deployment did not fail', async () => {
      const s = await getDeployStatus(client, project.id, deploymentId1)
      if (!s.ok) throw new Error(`deployment ${deploymentId1} is in state "${s.state}"`)
    })
    await step('wait for HTTP ready', () =>
      waitForHttpReady({ url: target.url, headers: target.headers, timeoutMs: 120_000 }),
    )
    await step('assert not console fallback', () =>
      assertNotConsoleFallback({ url: target.url, headers: target.headers }),
    )
    await step('assert marker key is absent before any env var exists', () =>
      waitForEnvValue(target, markerKey, undefined, {
        label: 'echo-server env before create',
        timeoutMs: 20_000,
      }),
    )

    // --- 2. create the env var, scoped to production only ---
    const varId = await step('create env var (scoped to production)', async () => {
      const res = unwrap(
        await createEnvironmentVariable({
          client,
          path: { project_id: project.id },
          body: { key: markerKey, value: valueA, environment_ids: [env.id] },
        }),
        'createEnvironmentVariable',
      )
      if (res.value !== valueA) {
        throw new Error(
          `create response did not round-trip the plaintext value: expected ${JSON.stringify(valueA)}, got ${JSON.stringify(res.value)}`,
        )
      }
      return res.id
    })
    envVarIds.push({ projectId: project.id, varId })
    log(`  env var #${varId} = ${valueA}`)

    // --- 3. redeploy, assert the RUNNING container actually sees it ---
    const deploymentId2 = await step('redeploy after create', () =>
      deployImage(client, { projectId: project.id, environmentId: env.id, imageRef: image }),
    )
    deployments.push({ projectId: project.id, deploymentId: deploymentId2 })
    log(`  deployment #${deploymentId2}`)

    await step('wait for redeploy (after create)', () =>
      waitForDeployment(client, {
        projectId: project.id,
        deploymentId: deploymentId2,
        timeoutMs: deployTimeoutMs,
        onPoll: (s) => log(`    ...${s.state}`),
      }),
    )
    await step('confirm redeploy did not fail (after create)', async () => {
      const s = await getDeployStatus(client, project.id, deploymentId2)
      if (!s.ok) throw new Error(`deployment ${deploymentId2} is in state "${s.state}"`)
    })
    await step(
      'assert the RUNNING container reflects the new env var (real propagation, not just a DB row)',
      () => waitForEnvValue(target, markerKey, valueA, { label: 'echo-server env after create + redeploy' }),
    )

    // --- 4. update the value, redeploy, assert the running container updated ---
    await step('update env var to a new value', async () => {
      const res = unwrap(
        await updateEnvironmentVariable({
          client,
          path: { project_id: project.id, var_id: varId },
          body: { key: markerKey, value: valueB, environment_ids: [env.id] },
        }),
        'updateEnvironmentVariable',
      )
      if (res.value !== valueB) {
        throw new Error(
          `update response did not round-trip the new plaintext value: expected ${JSON.stringify(valueB)}, got ${JSON.stringify(res.value)}`,
        )
      }
    })

    const deploymentId3 = await step('redeploy after update', () =>
      deployImage(client, { projectId: project.id, environmentId: env.id, imageRef: image }),
    )
    deployments.push({ projectId: project.id, deploymentId: deploymentId3 })
    log(`  deployment #${deploymentId3}`)

    await step('wait for redeploy (after update)', () =>
      waitForDeployment(client, {
        projectId: project.id,
        deploymentId: deploymentId3,
        timeoutMs: deployTimeoutMs,
        onPoll: (s) => log(`    ...${s.state}`),
      }),
    )
    await step('confirm redeploy did not fail (after update)', async () => {
      const s = await getDeployStatus(client, project.id, deploymentId3)
      if (!s.ok) throw new Error(`deployment ${deploymentId3} is in state "${s.state}"`)
    })
    await step(
      'assert the RUNNING container reflects the UPDATED value (not the stale original)',
      () => waitForEnvValue(target, markerKey, valueB, { label: 'echo-server env after update + redeploy' }),
    )

    // --- 5. delete the env var, redeploy, assert it's genuinely gone ---
    await step('delete env var', async () => {
      await deleteEnvironmentVariable({ client, path: { project_id: project.id, var_id: varId } })
    })
    // Deleted -- don't attempt to delete it again in teardown.
    envVarIds.splice(
      envVarIds.findIndex((v) => v.varId === varId),
      1,
    )

    const deploymentId4 = await step('redeploy after delete', () =>
      deployImage(client, { projectId: project.id, environmentId: env.id, imageRef: image }),
    )
    deployments.push({ projectId: project.id, deploymentId: deploymentId4 })
    log(`  deployment #${deploymentId4}`)

    await step('wait for redeploy (after delete)', () =>
      waitForDeployment(client, {
        projectId: project.id,
        deploymentId: deploymentId4,
        timeoutMs: deployTimeoutMs,
        onPoll: (s) => log(`    ...${s.state}`),
      }),
    )
    await step('confirm redeploy did not fail (after delete)', async () => {
      const s = await getDeployStatus(client, project.id, deploymentId4)
      if (!s.ok) throw new Error(`deployment ${deploymentId4} is in state "${s.state}"`)
    })
    await step(
      'assert the RUNNING container no longer has the deleted var (falls back to unset)',
      () => waitForEnvValue(target, markerKey, undefined, { label: 'echo-server env after delete + redeploy' }),
    )

    // --- 6. lightweight API-only check: environment scoping, no deploy needed ---
    const staging = await step('create a second (staging) environment', async () => {
      const res = unwrap(
        await createEnvironment({
          client,
          path: { project_id: project.id },
          body: { name: `staging-${runId}`, branch: `staging-${runId}` },
        }),
        'createEnvironment',
      )
      return { id: res.id }
    })
    stagingEnvId = staging.id
    log(`  staging env #${staging.id}`)

    const scopeVarId = await step('create env var scoped to production ONLY', async () => {
      const res = unwrap(
        await createEnvironmentVariable({
          client,
          path: { project_id: project.id },
          body: { key: scopeKey, value: scopeValue, environment_ids: [env.id] },
        }),
        'createEnvironmentVariable',
      )
      return res.id
    })
    envVarIds.push({ projectId: project.id, varId: scopeVarId })
    log(`  scope-test env var #${scopeVarId}`)

    await step('assert it does NOT appear when filtered by the staging environment', async () => {
      const vars = unwrap(
        await getEnvironmentVariables({
          client,
          path: { project_id: project.id },
          query: { environment_id: staging.id },
        }),
        'getEnvironmentVariables(staging)',
      )
      const found = vars.find((v) => v.id === scopeVarId)
      if (found) {
        throw new Error(
          `env var #${scopeVarId} (scoped to production only) was returned when filtering by staging env #${staging.id} -- environment scoping is not actually enforced`,
        )
      }
    })
    await step('assert it DOES appear when filtered by production', async () => {
      const vars = unwrap(
        await getEnvironmentVariables({
          client,
          path: { project_id: project.id },
          query: { environment_id: env.id },
        }),
        'getEnvironmentVariables(production)',
      )
      const found = vars.find((v) => v.id === scopeVarId)
      if (!found) {
        throw new Error(
          `env var #${scopeVarId} was NOT returned when filtering by its own environment (production env #${env.id})`,
        )
      }
    })

    await step('delete scope-test env var', async () => {
      await deleteEnvironmentVariable({ client, path: { project_id: project.id, var_id: scopeVarId } })
    })
    envVarIds.splice(
      envVarIds.findIndex((v) => v.varId === scopeVarId),
      1,
    )
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(`\n(kept resources: projects=${projectIds.join(',')} staging_env=${stagingEnvId ?? '-'})`)
    } else {
      const envVarErrors: string[] = []
      for (const v of envVarIds) {
        try {
          await deleteEnvironmentVariable({ client, path: { project_id: v.projectId, var_id: v.varId } })
        } catch (e) {
          envVarErrors.push(`deleteEnvironmentVariable(${v.varId}): ${(e as Error).message}`)
        }
      }
      // No explicit environment delete: `DELETE .../environments/{id}` is
      // gated behind `require_sensitive_action` (browser-session-only, see
      // temps-auth/src/sensitive_action.rs -- an API key is deliberately
      // denied), so it can never succeed from this API-key-driven harness.
      // `deleteProject` below cascades environments (and their env vars,
      // deployments) via `ON DELETE CASCADE` regardless, same as every
      // other scenario in this suite.
      const td = await teardown(client, { deployments, projectIds })
      log(
        `\n▶ teardown: deleted ${envVarIds.length - envVarErrors.length} env var(s), ` +
          `tore down ${td.teardownDeployments} deployment(s), ` +
          `deleted ${td.deletedProjects} project(s)` +
          (td.errors.length + envVarErrors.length ? ` (${td.errors.length + envVarErrors.length} errors)` : ''),
      )
      for (const e of [...envVarErrors, ...td.errors]) log(`    ! ${e}`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: EnvVarsScenarioResult = { runId, ok, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ env-vars-scenario PASSED' : '❌ env-vars-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
