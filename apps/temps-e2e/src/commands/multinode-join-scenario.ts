/**
 * The first-ever e2e test for temps's direct-underlay multi-node clustering
 * feature. Nothing tests this today — only Rust unit tests with
 * MockDatabase (`crates/temps-deployments/tests/multinode_integration_test.rs`)
 * and a manual, non-automated dev tool (`tools/dev-cluster/`).
 *
 * Every other scenario in this suite points `--url`/`--api-key` at an
 * ALREADY-RUNNING `temps serve` instance (started via the `start-temps`
 * skill) and drives it over HTTP. This one can't: it has to prove a SECOND,
 * genuinely separate node (its own Docker daemon, its own binary, its own
 * network identity) joins the first node's mesh — that needs its own
 * Docker-in-Docker (DinD) 2-node cluster with real single-use enrollment,
 * HTTPS transport, and mTLS registration. WireGuard relay enrollment remains
 * a separate topology,
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
 *      — this is the real proof `POST /internal/nodes/register` completed over
 *      trusted HTTPS as a genuine mTLS-registered join, not a mocked one, then
 *      assert that the stored node endpoint is HTTPS, cert material exists,
 *      legacy enrollment is disabled, and the single-use token was consumed.
 *      Bounded by the same generous timeout: the worker ALSO compiles its own
 *      binary from scratch on first boot.
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
 *  11. enable the opt-in cluster resolver, deploy a second unprivileged
 *      application in another project, and `docker exec` its `wget` against
 *      `production.<project>.temps.local`. DNS must resolve inside the real
 *      deployed container, but the internal proxy must reject the cross-project
 *      request with 403 rather than exposing the target application.
 *  12. clear the test's worker-only placement override, then drain the worker
 *      (`POST /internal/nodes/{id}/drain`) and poll
 *      `GET /internal/nodes/{id}/drain` until `drain_complete`, then
 *      re-run the same `docker ps` side-channel check on both containers —
 *      in this 2-node cluster the container has nowhere to go but the
 *      control plane, so this also implicitly re-tests the `Local`
 *      fallback scheduling path.
 *  13. remove the worker node (`DELETE /internal/nodes/{id}`) and confirm
 *      it's gone from `GET /internal/nodes`.
 *  14. teardown (in a `finally`, matching every other scenario's
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
import { mkdir } from 'node:fs/promises'
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
  teardown,
  makeRunId,
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
  'temps-e2e-mn-bootstrap-state',
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
  timeoutMs?: number,
): Promise<void> {
  const proc = Bun.spawn(args, { stdout: 'pipe', stderr: 'pipe' })
  const outputTail: string[] = []
  let timedOut = false
  const timeout = timeoutMs === undefined
    ? undefined
    : setTimeout(() => {
        timedOut = true
        proc.kill()
      }, timeoutMs)
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
      for (const l of lines) {
        if (!l.trim()) continue
        onLog(l)
        outputTail.push(l)
        if (outputTail.length > 60) outputTail.shift()
      }
    }
    if (buf.trim()) {
      onLog(buf)
      outputTail.push(buf)
      if (outputTail.length > 60) outputTail.shift()
    }
  }
  const [, , code] = await Promise.all([pump(proc.stdout), pump(proc.stderr), proc.exited])
  if (timeout !== undefined) clearTimeout(timeout)
  if (timedOut) {
    throw new Error(`${what} did not finish within ${Math.round(timeoutMs! / 1000)}s and was terminated`)
  }
  if (code !== 0) {
    const detail = outputTail.length ? `\n${outputTail.join('\n')}` : ''
    throw new Error(`${what} failed (exit ${code})${detail}`)
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

/** Wait until the proxy serves the actual whoami container, not a transient 404/console route. */
async function waitForWhoami(
  target: ReturnType<typeof resolveLoadTarget>,
  timeoutMs = 60_000,
): Promise<void> {
  await pollUntil(
    async () => {
      try {
        const res = await fetch(target.url, { headers: target.headers })
        return { status: res.status, body: (await res.text()).slice(0, 200) }
      } catch (error) {
        return { status: 0, body: (error as Error).message }
      }
    },
    (response) => response.status === 200 && response.body.includes('Hostname'),
    {
      timeoutMs,
      intervalMs: 1000,
      label: 'control-plane proxy to serve the traefik/whoami deployment',
    },
  )
}

export async function multinodeJoinScenarioCommand(opts: MultinodeJoinScenarioOptions): Promise<void> {
  const composeFile = opts.composeFile ?? DEFAULT_COMPOSE_FILE
  const buildTimeoutMs = Number(opts.buildTimeout ?? '1800000')
  if (!Number.isFinite(buildTimeoutMs) || buildTimeoutMs <= 0) {
    throw new Error(`--build-timeout must be a positive number of milliseconds, got "${opts.buildTimeout}"`)
  }
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
  let clusterStartAttempted = false
  const revokeTemporaryApiKey = async () => {
    if (!clusterStartAttempted) return
    const revoke = await runCaptured([
      'docker',
      'exec',
      'temps-e2e-mn-postgres',
      'psql',
      '-U',
      'temps',
      '-d',
      'temps',
      '-c',
      "UPDATE api_keys SET is_active = false, updated_at = now() WHERE name = 'e2e-multinode' AND is_active = true",
    ])
    if (revoke.code !== 0) log('    ! temporary API-key revocation failed; cluster teardown will remove its database')
  }

  try {
    await step('remove any stale multinode E2E topology and identity state', async () => {
      const down = await runCaptured([...composeArgs, 'down'])
      if (down.code !== 0) {
        throw new Error(`preflight docker compose down failed: ${down.stderr.trim()}`)
      }
      for (const volume of IDENTITY_VOLUMES) {
        // Missing volumes are expected on a genuinely fresh run.
        await runCaptured(['docker', 'volume', 'rm', '-f', volume])
      }
      // These named volumes are mounted below the read-only /workspace bind.
      // runc cannot create their mountpoints through that parent on a clean
      // checkout, so make the gitignored directories before Compose starts.
      await Promise.all([
        mkdir(path.join(REPO_ROOT, 'target'), { recursive: true }),
        mkdir(path.join(REPO_ROOT, 'crates/temps-cli/dist'), { recursive: true }),
      ])
    })

    await step(
      `bring up the 2-node cluster (docker compose up -d --build, bounded ${Math.round(buildTimeoutMs / 1000)}s — first-run compile is ~15-20 min, this is NOT hung)`,
      async () => {
        const t0 = performance.now()
        clusterStartAttempted = true
        await runStreamed(
          [...composeArgs, 'up', '-d', '--build'],
          (line) => log(`    [compose] ${line}`),
          'docker compose up -d --build',
          buildTimeoutMs,
        )
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
        throw new Error(`temps api-key exited ${res.code}; credential-bearing output suppressed`)
      }
      const jsonStart = res.stdout.indexOf('{')
      if (jsonStart === -1) {
        throw new Error('temps api-key produced no JSON; credential-bearing output suppressed')
      }
      let parsed: { api_key: string }
      try {
        parsed = JSON.parse(res.stdout.slice(jsonStart))
      } catch (e) {
        throw new Error(`temps api-key produced unparsable JSON: ${(e as Error).message}; credential-bearing output suppressed`)
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
          // `temps join` creates the node before the agent has started with
          // its issued leaf certificate. The first agent heartbeat upgrades
          // the provisional HTTP address to its final HTTPS endpoint, so wait
          // for both states instead of racing that hand-off.
          (n) => n !== undefined && n.status === 'active' && n.address.startsWith('https://'),
          {
            timeoutMs: buildTimeoutMs,
            intervalMs: 5000,
            onPoll: (n) =>
              log(
                `    ...${WORKER_NAME}=${n ? `${n.status} ${n.address}` : '(not registered yet)'}`,
              ),
            label: `node '${WORKER_NAME}' to register, report active, and publish its HTTPS agent endpoint`,
          },
        )
        return node!.id
      },
    )
    log(`  worker node id=${workerNodeId}`)

    await step('verify worker registration negotiated mTLS', async () => {
      const nodes = unwrap(await adminListNodes({ client: client! }), 'adminListNodes')
      const worker = nodes.nodes.find((node) => node.id === workerNodeId)
      if (!worker?.address.startsWith('https://')) {
        throw new Error(`worker address is ${worker?.address ?? '(missing)'}, expected an https:// mTLS endpoint`)
      }
      const certCheck = await runCaptured([
        'docker',
        'exec',
        WORKER_CONTAINER,
        'bash',
        '-c',
        'test -s /root/.temps/node.cert.pem && test -s /root/.temps/node.key.pem && test -s /root/.temps/cluster-ca.pem',
      ])
      if (certCheck.code !== 0) {
        throw new Error('worker did not persist its mTLS leaf, private key, and cluster CA')
      }
      const posture = await runCaptured([
        'docker',
        'exec',
        'temps-e2e-mn-postgres',
        'psql',
        '-At',
        '-U',
        'temps',
        '-d',
        'temps',
        '-c',
        "SELECT data::jsonb->'multi_node'->>'require_mtls', data::jsonb->'multi_node'->>'legacy_shared_token_enabled', data::jsonb->'cluster_dns'->>'enabled', used_count, max_uses FROM settings CROSS JOIN node_enrollment_tokens WHERE settings.id = 1",
      ])
      if (posture.code !== 0 || posture.stdout.trim() !== 'true|false|true|1|1') {
        throw new Error(`unexpected enrollment posture: ${posture.stdout.trim() || posture.stderr.trim() || '(no output)'}`)
      }
    })

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
          return workerContainers.some((n) => n.includes(project.slug))
        },
        (found) => found,
        {
          timeoutMs: 20_000,
          intervalMs: 1000,
          onPoll: (found) => log(`    ...container on worker-1=${found}`),
          label: "deployment container to appear on worker-1's docker ps",
        },
      )
      const onControlPlane = cpContainers.some((n) => n.includes(project.slug))
      if (onControlPlane) {
        throw new Error(
          `deployment container unexpectedly found on the control plane's docker ps (containers: ${cpContainers.join(', ') || '(none)'}) -- target_nodes pinning did not take effect`,
        )
      }
      log(`  worker-1 docker ps: ${workerContainers.join(', ')}`)
    })

    const target = resolveLoadTarget(cfg.url, env.mainUrl)
    await step('real HTTP proof of life through the control-plane proxy', () => waitForWhoami(target))

    const dnsClientProject = await step('create a second application for app-to-app DNS', () =>
      createE2eProject(client!, { name: `${runId}-dns-client`, exposedPort: 8080 }),
    )
    projectIds.push(dnsClientProject.id)
    const dnsClientEnv = await step('resolve the DNS client production environment', () =>
      getProductionEnvironment(client!, dnsClientProject.id),
    )
    await step('pin the DNS client application to the worker', async () => {
      unwrap(
        await updateEnvironmentSettings({
          client: client!,
          path: { project_id: dnsClientProject.id, env_id: dnsClientEnv.id },
          body: { target_nodes: [workerNodeId!] },
        }),
        'updateEnvironmentSettings',
      )
    })
    const dnsClientDeploymentId = await step('deploy unprivileged Nginx as the DNS client application', () =>
      deployFromImage({
        client: client!,
        path: { project_id: dnsClientProject.id, environment_id: dnsClientEnv.id },
        body: { image_ref: 'nginxinc/nginx-unprivileged:alpine' },
      }).then((res) => unwrap(res, 'deployFromImage').id),
    )
    deployments.push({ projectId: dnsClientProject.id, deploymentId: dnsClientDeploymentId })
    await step('wait for the DNS client deployment on the worker', async () => {
      await waitForDeployment(client!, {
        projectId: dnsClientProject.id,
        deploymentId: dnsClientDeploymentId,
        timeoutMs: 300_000,
        onPoll: (status) => log(`    ...${status.state}`),
      })
      const status = await getDeployStatus(client!, dnsClientProject.id, dnsClientDeploymentId)
      if (!status.ok) throw new Error(`DNS client deployment ${dnsClientDeploymentId} is in state "${status.state}"`)
    })

    await step('prove *.temps.local DNS rejects cross-project application access', async () => {
      const clientContainer = await pollUntil(
        async () => (await dockerPsNames(WORKER_CONTAINER)).find((name) => name.includes(dnsClientProject.slug)),
        (name) => name !== undefined,
        { timeoutMs: 30_000, intervalMs: 1000, label: 'DNS client container to appear on the worker' },
      )
      const appUrl = `http://production.${project.slug}.temps.local`
      const request = await pollUntil(
        () =>
          runCaptured([
            'docker',
            'exec',
            WORKER_CONTAINER,
            'docker',
            'exec',
            clientContainer!,
            'wget',
            '-qO-',
            appUrl,
          ]),
        (result) => result.code !== 0 && result.stderr.includes('403 Forbidden'),
        {
          timeoutMs: 60_000,
          intervalMs: 2000,
          onPoll: (result) => log(`    ...wget exit=${result.code}`),
          label: `worker application to resolve ${appUrl} and receive the cross-project 403`,
        },
      )
      if (!request.stderr.includes('403 Forbidden')) {
        throw new Error(
          `worker application did not receive the expected cross-project 403 from ${appUrl}: ${request.stderr || request.stdout}`,
        )
      }
      log(`  ${clientContainer} -> ${appUrl}: cross-project access blocked`)
    })

    await step('remove the temporary DNS client application', async () => {
      const cleanup = await teardown(client!, {
        deployments: [{ projectId: dnsClientProject.id, deploymentId: dnsClientDeploymentId }],
        projectIds: [dnsClientProject.id],
      })
      if (cleanup.errors.length) throw new Error(cleanup.errors.join('; '))
      deployments.splice(deployments.findIndex((d) => d.deploymentId === dnsClientDeploymentId), 1)
      projectIds.splice(projectIds.indexOf(dnsClientProject.id), 1)
    })

    await step('clear the worker-only placement override before draining', async () => {
      unwrap(
        await updateEnvironmentSettings({
          client: client!,
          path: { project_id: project.id, env_id: env.id },
          body: { target_nodes: [] },
        }),
        'updateEnvironmentSettings',
      )
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
      // `drain_complete` means the source node has no remaining containers
      // and is safe to remove. The replacement deployment is queued
      // asynchronously, so its Docker container can appear a few seconds
      // later. Poll the actual data plane instead of racing that queue.
      let workerContainers: string[] = []
      let cpContainers: string[] = []
      await pollUntil(
        async () => {
          ;[workerContainers, cpContainers] = await Promise.all([
            dockerPsNames(WORKER_CONTAINER),
            dockerPsNames(CONTROL_PLANE_CONTAINER),
          ])
          return {
            onWorker: workerContainers.some((n) => n.includes(project.slug)),
            onControlPlane: cpContainers.some((n) => n.includes(project.slug)),
          }
        },
        (placement) => !placement.onWorker && placement.onControlPlane,
        {
          timeoutMs: 60_000,
          intervalMs: 1000,
          onPoll: (placement) =>
            log(
              `    ...post-drain placement worker=${placement.onWorker} control-plane=${placement.onControlPlane}`,
            ),
          label: 'replacement container to become visible on the control plane after drain',
        },
      )
      const onWorker = workerContainers.some((n) => n.includes(project.slug))
      const onControlPlane = cpContainers.some((n) => n.includes(project.slug))
      if (onWorker) {
        throw new Error(
          `deployment container still present on worker-1's docker ps after drain (containers: ${workerContainers.join(', ')})`,
        )
      }
      if (!onControlPlane) {
        throw new Error(
          `deployment container was not recreated on the control plane after drain (containers: ${cpContainers.join(', ') || '(none)'})`,
        )
      }
      log(`  post-drain: worker-1=[${workerContainers.join(', ')}] control-plane=[${cpContainers.join(', ')}]`)

      await waitForWhoami(target)
    })

    await step('remove the worker node', async () => {
      unwrap(
        await adminRemoveNode({ client: client!, path: { node_id: workerNodeId! } }),
        'adminRemoveNode',
      )
    })

    await step('reject replay of the consumed enrollment token after node removal', async () => {
      const replay = await runCaptured([
        'docker',
        'exec',
        WORKER_CONTAINER,
        'bash',
        '-c',
        'read -r token < /run/temps-bootstrap/join_token.txt; TEMPS_JOIN_TOKEN="$token" /usr/local/bin/temps join https://control-plane.temps.test "$token" --name worker-1 --private-address 10.52.0.21 --agent-address 0.0.0.0:3100',
      ])
      if (replay.code === 0) {
        throw new Error('consumed single-use enrollment token unexpectedly registered the removed node again')
      }
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
      await revokeTemporaryApiKey()
      log(`\n(kept: entire 2-node cluster left running -- 'docker compose -f ${composeFile} -p ${COMPOSE_PROJECT} down' to stop it)`)
    } else {
      // Best-effort SDK-level teardown first (project/deployments), then tear
      // down the cluster itself.
      if (client) {
        const td = await teardown(client, { deployments, projectIds })
        log(
          `\n▶ teardown: tore down ${td.teardownDeployments} deployment(s), deleted ${td.deletedProjects} project(s), deleted ${td.deletedServices} service(s)` +
            (td.errors.length ? ` (${td.errors.length} errors)` : ''),
        )
        for (const e of td.errors) log(`    ! ${e}`)
      }
      await revokeTemporaryApiKey()
      if (clusterStartAttempted) {
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
