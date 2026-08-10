/**
 * The first-ever e2e test for temps's multi-node/WireGuard clustering
 * feature. Nothing tests this today — only Rust unit tests with
 * MockDatabase (`crates/temps-deployments/tests/multinode_integration_test.rs`)
 * and a manual, non-automated dev tool (`tools/dev-cluster/`).
 *
 * Every other scenario in this suite points `--url`/`--api-key` at an
 * ALREADY-RUNNING `temps serve` instance (started via the `start-temps`
 * skill) and drives it over HTTP. This one can't: it has to prove a SECOND,
 * genuinely separate node (its own Docker daemon, its own binary, its own
 * network identity) joins the first node's mesh — that needs its own
 * Docker-in-Docker (DinD) 2-node cluster with real WireGuard/mTLS
 * registration, exactly what `tools/dev-cluster/` already proves works,
 * just not automated or asserted against. So this scenario does NOT use the
 * global `--url`/`--api-key`/`connection()` the way every other scenario
 * does (the one precedent for a scenario driving its own topology is
 * `tls-scenario`'s dedicated fixed-port Pebble instance, taken further
 * here: this scenario owns its ENTIRE cluster, brings it up, mints its own
 * credentials, asserts against it, tears it down).
 *
 * Steps:
 *   1. `docker compose up -d --build` the 2-node cluster
 *      (tools/e2e-multinode-cluster/docker-compose.yml — a trimmed,
 *      re-subnetted clone of tools/dev-cluster/docker-compose.yml; see that
 *      file's own header comment for why it's safe to run alongside a real
 *      dev-cluster instance).
 *   2. poll `docker inspect` for the control-plane container's health
 *      status until `healthy` — first boot compiles the full Rust
 *      workspace from source inside the container, so this is bounded by a
 *      generous `--build-timeout` (default 30 min), not the usual
 *      30s-90s window other scenarios use.
 *   3. mint an admin API key directly from the DB
 *      (`docker exec ... temps api-key --database-url=...`), the same
 *      DB-direct minting pattern `db-apikey.ts`/`rbac-scenario` use — this
 *      works because `role-control-plane.sh`'s `temps setup --auto`
 *      guarantees `admin@local.dev` exists by the time the healthcheck
 *      passes.
 *   4. build `cfg = { url: 'http://localhost:18180', apiKey }` and drive
 *      everything from here on through the normal `@temps-sdk/api` client,
 *      same as every other scenario.
 *   5. poll `GET /internal/nodes` until a node named `worker-1` (the
 *      `WORKER_NAME` the compose file sets) appears with `status: "active"`
 *      — this is the real proof `POST /internal/nodes/register` completed
 *      a genuine mTLS-registered join, not a mocked one. Bounded by the
 *      same generous timeout: the worker ALSO compiles its own binary from
 *      scratch on first boot.
 *   6. create a throwaway project + resolve its production environment.
 *   7. `PUT .../environments/{id}/settings` with `target_nodes: [worker
 *      node id]` — pins every future deploy in this environment to the
 *      worker, never the control plane.
 *   8. deploy `traefik/whoami:latest` (same image other scenarios in this
 *      repo already use for basic deploys) and poll it to healthy.
 *   9. the core assertion: `docker exec` into BOTH containers and confirm
 *      the deployed container's name shows up in `worker-1`'s `docker ps`
 *      and NOT in the control plane's. See `ensure_image_on_remote` in
 *      `crates/temps-deployments/src/jobs/deploy_image.rs` for how this
 *      actually works without a registry: the control plane's own
 *      `DockerImageBuilder::save_image` pulls (if needed) and `docker
 *      save`s the image to a tar, streams it to the worker's agent
 *      (`POST /agent/images/import`), and the agent `docker load`s it —
 *      that's why a bare public image tag works here with zero registry
 *      setup, unlike most other scenarios in this suite that need
 *      `--registry`.
 *  10. real HTTP proof of life: hit the deployed app through the
 *      control-plane's proxy (localhost:18180) with the app's Host header
 *      and assert a real response body, not just a healthy status field.
 *  11. drain the worker (`POST /internal/nodes/{id}/drain`), poll
 *      `GET /internal/nodes/{id}/drain` until `drain_complete`, then
 *      re-run the same `docker ps` side-channel check on both containers —
 *      in this 2-node cluster the container has nowhere to go but the
 *      control plane, so this also implicitly re-tests the `Local`
 *      fallback scheduling path.
 *  12. remove the worker node (`DELETE /internal/nodes/{id}`) and confirm
 *      it's gone from `GET /internal/nodes`.
 *  13. teardown (in a `finally`, matching every other scenario's
 *      discipline): `docker compose down` (no `-v`, so the cache volumes —
 *      cargo registry/git + workspace target/ — survive for a fast
 *      re-run), then explicitly `docker volume rm` the identity/state
 *      volumes (postgres data, both containers' `/var/lib/docker` +
 *      `/var/lib/temps`, the worker's `/root`) so the NEXT run proves a
 *      genuinely fresh join rather than skipping registration via one of
 *      the role scripts' own marker files. `--keep` skips all of this —
 *      unlike every other scenario, that leaves an entire running 2-node
 *      cluster behind, not just one container.
 */
import { fileURLToPath } from 'node:url'
import path from 'node:path'
import {
  updateEnvironmentSettings,
  deployFromImage,
  adminListNodes,
  adminDrainNode,
  adminDrainStatus,
  adminRemoveNode,
} from '@temps-sdk/api'
import { makeClient, unwrap } from '../lib/client.ts'
import {
  createE2eProject,
  getProductionEnvironment,
  waitForDeployment,
  getDeployStatus,
  resolveLoadTarget,
  assertNotConsoleFallback,
  teardown,
  makeRunId,
  sleep,
  pollUntil,
} from '../lib/flows.ts'
import type { Client } from '@temps-sdk/api/client'

// apps/temps-e2e/src/commands/ -> repo root is 4 levels up.
const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../..')
const DEFAULT_COMPOSE_FILE = path.join(REPO_ROOT, 'tools/e2e-multinode-cluster/docker-compose.yml')
const COMPOSE_PROJECT = 'temps-e2e-mn'
const CONTROL_PLANE_CONTAINER = 'temps-e2e-mn-control-plane'
const WORKER_CONTAINER = 'temps-e2e-mn-worker-1'
const WORKER_NAME = 'worker-1'
const CONTROL_PLANE_URL = 'http://localhost:18180'
const POSTGRES_DIRECT_URL = 'postgres://temps:temps@10.52.0.5:5432/temps'
const IDENTITY_VOLUMES = [
  'temps-e2e-mn-postgres-data',
  'temps-e2e-mn-cp-docker',
  'temps-e2e-mn-cp-data',
  'temps-e2e-mn-worker1-docker',
  'temps-e2e-mn-worker1-data',
  'temps-e2e-mn-worker1-home',
]

export interface MultinodeJoinScenarioOptions {
  composeFile?: string
  buildTimeout?: string
  keep?: boolean
  json?: boolean
}

interface StepLog {
  step: string
  ok: boolean
  detail?: string
  ms?: number
}

interface MultinodeJoinScenarioResult {
  runId: string
  ok: boolean
  workerNodeId?: number
  steps: StepLog[]
}

/** Run a subcommand, streaming stdout/stderr line-by-line through onLog; throws on non-zero exit. */
async function runStreamed(
  args: string[],
  onLog: (line: string) => void,
  what: string,
): Promise<void> {
  const proc = Bun.spawn(args, { stdout: 'pipe', stderr: 'pipe' })
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
      for (const l of lines) if (l.trim()) onLog(l)
    }
    if (buf.trim()) onLog(buf)
  }
  await Promise.all([pump(proc.stdout), pump(proc.stderr)])
  const code = await proc.exited
  if (code !== 0) {
    throw new Error(`${what} failed (exit ${code})`)
  }
}

/** Run a subcommand to completion and collect its output (no streaming). */
async function runCaptured(
  args: string[],
): Promise<{ code: number; stdout: string; stderr: string }> {
  const proc = Bun.spawn(args, { stdout: 'pipe', stderr: 'pipe' })
  const [stdout, stderr, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ])
  return { code, stdout, stderr }
}

/** `docker inspect --format '{{.State.Health.Status}}' <name>`. Returns '' if the container doesn't exist yet. */
async function containerHealthStatus(containerName: string): Promise<string> {
  const res = await runCaptured([
    'docker',
    'inspect',
    '--format',
    '{{.State.Health.Status}}',
    containerName,
  ])
  if (res.code !== 0) return ''
  return res.stdout.trim()
}

/** `docker exec <container> docker ps --format '{{.Names}}'` — lists container names on that node's OWN Docker daemon (DinD). */
async function dockerPsNames(containerName: string): Promise<string[]> {
  const res = await runCaptured(['docker', 'exec', containerName, 'docker', 'ps', '--format', '{{.Names}}'])
  if (res.code !== 0) {
    throw new Error(`docker exec ${containerName} docker ps failed: ${res.stderr.trim()}`)
  }
  return res.stdout
    .split('\n')
    .map((l) => l.trim())
    .filter(Boolean)
}

export async function multinodeJoinScenarioCommand(opts: MultinodeJoinScenarioOptions): Promise<void> {
  const composeFile = opts.composeFile ?? DEFAULT_COMPOSE_FILE
  const buildTimeoutMs = Number(opts.buildTimeout ?? '1800000')
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps multinode-join scenario  ->  dedicated 2-node cluster (${composeFile})`)

  const composeArgs = ['docker', 'compose', '-f', composeFile, '-p', COMPOSE_PROJECT]

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

  let client: Client | undefined
  let cfg: { url: string; apiKey: string } | undefined
  const projectIds: number[] = []
  const deployments: { projectId: number; deploymentId: number }[] = []
  let workerNodeId: number | undefined
  let clusterUp = false

  try {
    await step(
      `bring up the 2-node cluster (docker compose up -d --build, bounded ${Math.round(buildTimeoutMs / 1000)}s — first-run compile is ~15-20 min, this is NOT hung)`,
      async () => {
        const t0 = performance.now()
        const run = runStreamed(
          [...composeArgs, 'up', '-d', '--build'],
          (line) => log(`    [compose] ${line}`),
          'docker compose up -d --build',
        )
        const timeout = sleep(buildTimeoutMs).then(() => {
          throw new Error(`docker compose up did not finish within ${Math.round(buildTimeoutMs / 1000)}s`)
        })
        await Promise.race([run, timeout])
        clusterUp = true
        log(`    cluster containers started in ${((performance.now() - t0) / 1000).toFixed(1)}s (images may still be compiling in the background)`)
      },
    )

    await step(
      `wait for control-plane container to report healthy (bounded ${Math.round(buildTimeoutMs / 1000)}s — compiles the full Rust workspace from source on first boot)`,
      () =>
        pollUntil(
          () => containerHealthStatus(CONTROL_PLANE_CONTAINER),
          (status) => status === 'healthy',
          {
            timeoutMs: buildTimeoutMs,
            intervalMs: 5000,
            onPoll: (status) => log(`    ...control-plane health=${status || '(container not found yet)'}`),
            label: 'control-plane container to report health=healthy',
          },
        ),
    )

    const apiKey = await step('mint an admin API key directly from the DB (docker exec ... temps api-key)', async () => {
      const res = await runCaptured([
        'docker',
        'exec',
        CONTROL_PLANE_CONTAINER,
        '/usr/local/bin/temps',
        'api-key',
        `--database-url=${POSTGRES_DIRECT_URL}`,
        '--name=e2e-multinode',
        '--role=admin',
        '--user-email=admin@local.dev',
        '--output-format=json',
      ])
      if (res.code !== 0) {
        throw new Error(`temps api-key exited ${res.code}\nstdout: ${res.stdout}\nstderr: ${res.stderr}`)
      }
      const jsonStart = res.stdout.indexOf('{')
      if (jsonStart === -1) {
        throw new Error(`temps api-key produced no JSON on stdout:\n${res.stdout}`)
      }
      let parsed: { api_key: string }
      try {
        parsed = JSON.parse(res.stdout.slice(jsonStart))
      } catch (e) {
        throw new Error(`temps api-key produced unparsable JSON: ${(e as Error).message}\nstdout: ${res.stdout}`)
      }
      return parsed.api_key
    })

    cfg = { url: CONTROL_PLANE_URL, apiKey }
    client = makeClient(cfg)
    log(`  cfg: ${cfg.url}`)

    workerNodeId = await step(
      `wait for worker '${WORKER_NAME}' to register + go active (bounded ${Math.round(buildTimeoutMs / 1000)}s — the worker also compiles its own binary from scratch)`,
      async () => {
        const node = await pollUntil(
          async () => {
            const res = await adminListNodes({ client: client! })
            if (res.error || !res.data) return undefined
            return res.data.nodes.find((n) => n.name === WORKER_NAME)
          },
          (n) => n !== undefined && n.status === 'active',
          {
            timeoutMs: buildTimeoutMs,
            intervalMs: 5000,
            onPoll: (n) => log(`    ...${WORKER_NAME}=${n ? n.status : '(not registered yet)'}`),
            label: `node '${WORKER_NAME}' to register and report status=active`,
          },
        )
        return node!.id
      },
    )
    log(`  worker node id=${workerNodeId}`)

    const project = await step('create a throwaway project', () =>
      createE2eProject(client!, { name: `${runId}-mn`, exposedPort: 80 }),
    )
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    const env = await step('resolve production environment', () => getProductionEnvironment(client!, project.id))
    log(`  env #${env.id} (${env.name})`)

    await step(`pin deploys to the worker (PUT .../settings target_nodes=[${workerNodeId}])`, async () => {
      unwrap(
        await updateEnvironmentSettings({
          client: client!,
          path: { project_id: project.id, env_id: env.id },
          body: { target_nodes: [workerNodeId!] },
        }),
        'updateEnvironmentSettings',
      )
    })

    const deploymentId = await step('deploy traefik/whoami:latest', () =>
      deployFromImage({
        client: client!,
        path: { project_id: project.id, environment_id: env.id },
        body: { image_ref: 'traefik/whoami:latest' },
      }).then((res) => unwrap(res, 'deployFromImage').id),
    )
    deployments.push({ projectId: project.id, deploymentId })
    log(`  deployment #${deploymentId}`)

    await step('wait for deployment (real docker save/tar transfer to the worker agent, no registry needed)', () =>
      waitForDeployment(client!, {
        projectId: project.id,
        deploymentId,
        timeoutMs: 300_000,
        onPoll: (s) => log(`    ...${s.state}`),
      }),
    )

    await step('confirm deployment did not fail', async () => {
      const s = await getDeployStatus(client!, project.id, deploymentId)
      if (!s.ok) throw new Error(`deployment ${deploymentId} is in state "${s.state}"`)
    })

    await step("assert the container landed on worker-1's own Docker daemon, NOT the control plane's", async () => {
      // The deployment API reports "healthy" once the proxy route is
      // registered, which can land a moment before the container is
      // actually created/started on the REMOTE worker's own Docker daemon
      // (image transfer + docker load/create/start all happen after the
      // route is wired) -- so this polls briefly rather than checking once.
      let workerContainers: string[] = []
      let cpContainers: string[] = []
      await pollUntil(
        async () => {
          ;[workerContainers, cpContainers] = await Promise.all([
            dockerPsNames(WORKER_CONTAINER),
            dockerPsNames(CONTROL_PLANE_CONTAINER),
          ])
          return workerContainers.some((n) => n.includes(runId))
        },
        (found) => found,
        {
          timeoutMs: 20_000,
          intervalMs: 1000,
          onPoll: (found) => log(`    ...container on worker-1=${found}`),
          label: "deployment container to appear on worker-1's docker ps",
        },
      )
      const onControlPlane = cpContainers.some((n) => n.includes(runId))
      if (onControlPlane) {
        throw new Error(
          `deployment container unexpectedly found on the control plane's docker ps (containers: ${cpContainers.join(', ') || '(none)'}) -- target_nodes pinning did not take effect`,
        )
      }
      log(`  worker-1 docker ps: ${workerContainers.join(', ')}`)
    })

    const target = resolveLoadTarget(cfg.url, env.mainUrl)
    await step('real HTTP proof of life through the control-plane proxy', async () => {
      await assertNotConsoleFallback({ url: target.url, headers: target.headers, timeoutMs: 60_000 })
      const res = await fetch(target.url, { headers: target.headers })
      const body = await res.text()
      if (res.status !== 200 || !body.includes('Hostname')) {
        throw new Error(
          `expected a real traefik/whoami response (status 200, body containing "Hostname"), got HTTP ${res.status}: ${body.slice(0, 200)}`,
        )
      }
    })

    await step('drain the worker', async () => {
      unwrap(
        await adminDrainNode({ client: client!, path: { node_id: workerNodeId! } }),
        'adminDrainNode',
      )
    })

    await step('poll drain status until complete', () =>
      pollUntil(
        async () =>
          unwrap(
            await adminDrainStatus({ client: client!, path: { node_id: workerNodeId! } }),
            'adminDrainStatus',
          ),
        (s) => s.drain_complete,
        {
          timeoutMs: 180_000,
          intervalMs: 3000,
          onPoll: (s) => log(`    ...drain_complete=${s.drain_complete} remaining=${s.remaining_containers}`),
          label: 'drain to complete',
        },
      ),
    )

    await step('confirm the container migrated off the worker (2-node cluster: only place left is the control plane)', async () => {
      const [workerContainers, cpContainers] = await Promise.all([
        dockerPsNames(WORKER_CONTAINER),
        dockerPsNames(CONTROL_PLANE_CONTAINER),
      ])
      const onWorker = workerContainers.some((n) => n.includes(runId))
      if (onWorker) {
        throw new Error(
          `deployment container still present on worker-1's docker ps after drain (containers: ${workerContainers.join(', ')})`,
        )
      }
      log(`  post-drain: worker-1=[${workerContainers.join(', ')}] control-plane=[${cpContainers.join(', ')}]`)
    })

    await step('remove the worker node', async () => {
      unwrap(
        await adminRemoveNode({ client: client!, path: { node_id: workerNodeId! } }),
        'adminRemoveNode',
      )
    })

    await step('confirm the node is gone from GET /internal/nodes', async () => {
      const res = unwrap(await adminListNodes({ client: client! }), 'adminListNodes')
      if (res.nodes.some((n) => n.id === workerNodeId)) {
        throw new Error(`node ${workerNodeId} still present in GET /internal/nodes after DELETE`)
      }
    })
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(`\n(kept: entire 2-node cluster left running -- 'docker compose -f ${composeFile} -p ${COMPOSE_PROJECT} down' to stop it)`)
    } else {
      // Best-effort SDK-level teardown first (project/deployments), then tear
      // down the cluster itself.
      if (client) {
        const td = await teardown(client, { deployments, projectIds })
        log(
          `\n▶ teardown: tore down ${td.teardownDeployments} deployment(s), deleted ${td.deletedProjects} project(s)` +
            (td.errors.length ? ` (${td.errors.length} errors)` : ''),
        )
        for (const e of td.errors) log(`    ! ${e}`)
      }
      if (clusterUp) {
        log('\n▶ teardown: docker compose down (preserving cache volumes for a fast re-run)')
        try {
          await runStreamed(
            [...composeArgs, 'down'],
            (line) => log(`    [compose] ${line}`),
            'docker compose down',
          )
        } catch (e) {
          log(`    ! docker compose down failed: ${(e as Error).message}`)
        }
        log('▶ teardown: removing identity/state volumes so the next run proves a genuinely fresh join')
        for (const vol of IDENTITY_VOLUMES) {
          const res = await runCaptured(['docker', 'volume', 'rm', '-f', vol])
          if (res.code !== 0) {
            log(`    ! docker volume rm ${vol} failed: ${res.stderr.trim()}`)
          }
        }
      }
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: MultinodeJoinScenarioResult = { runId, ok, workerNodeId, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ multinode-join-scenario PASSED' : '❌ multinode-join-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
