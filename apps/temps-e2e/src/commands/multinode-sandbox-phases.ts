// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Sandboxes on worker nodes (ADR-048 §13), run inside
 * `multinode-join-scenario` against its real 2-node mTLS cluster.
 *
 * Runs after the scenario has drained its deployments off the worker and
 * before it removes the worker, so:
 *   - the worker hosts no deployment containers, and `DELETE
 *     /internal/nodes/{id}` can only be refused because of sandboxes (the
 *     handler checks containers first);
 *   - restarting the control plane / freezing the worker cannot disturb an
 *     application the outer scenario still asserts on;
 *   - the outer scenario's own final "remove the worker node" step proves
 *     that, once evicted, sandboxes no longer block removal.
 *
 * Phases (each a PASS/FAIL step line in the scenario's output):
 *   1. reactivate the drained worker (DELETE /internal/nodes/{id}/drain)
 *   2. GET /v1/sandboxes/placement lists the worker as eligible
 *   3. PUT allowed_node_ids [worker] then back to null
 *   4. pre-pull the sandbox image on the worker (best effort; the provider
 *      would otherwise pull or build it during the first create)
 *   5. create with `node: worker-1` -> node_id/node_name of the worker
 *   6. `temps-sandbox-<label>` runs on the worker's Docker, not the control plane's
 *   7. exec `echo` + `hostname` (hostname = the worker container's hostname)
 *   8. write a file, read it back; it is in the worker's work dir on the host
 *   9. pause (stop) -> resume (start) -> exec works and the file survived
 *  10. restart the control-plane container; exec still works (lazy recovery)
 *  11. snapshot is refused with 422 and the sandbox keeps running
 *  12. allow-list [0] + `node: worker-1` -> 422 sandbox-node-not-allowed
 *  13. allow-list [worker] + no `node` -> default placement lands on the worker
 *  14. `docker pause` the worker until it is offline -> exec 503 naming it;
 *      unpause, it comes back, exec works again
 *  15. DELETE the worker node -> 409; drain status remaining_sandboxes >= 1
 *  16. destroy the second sandbox -> container and worker work dir gone
 *  17. evict the worker -> destroyed lists the sandbox, nothing unconfirmed,
 *      no `temps-sandbox-` container left, remaining_sandboxes 0
 *  18. drain the worker again; drain status allows removal
 * Cleanup (always): unpause the worker, destroy any sandbox still tracked,
 * restore allowed_node_ids to null.
 */
import { adminDrainNode, adminDrainStatus, adminListNodes, adminRemoveNode, adminUndrainNode, getGlobalSandboxStatus } from '@temps-sdk/api'
import type { Client } from '@temps-sdk/api/client'
import { unwrap, type TempsClientConfig } from '../lib/client.ts'
import { pollUntil } from '../lib/flows.ts'
import {
  SANDBOX_PROBLEM_TYPES,
  SANDBOX_WORKSPACE_DIR,
  SandboxApi,
  describeResult,
  evictionIssues,
  expectProblem,
  expectStatus,
  findPlacementNode,
  fromBase64,
  parseDockerNames,
  parseMarkerAndHostname,
  isNewerTimestamp,
  parseProblem,
  sandboxContainerName,
  sandboxContainersIn,
  sandboxImageFallbacks,
  workerWorkDirCandidates,
  type SandboxInner,
  type SandboxPlacement,
} from '../lib/sandbox-api.ts'

type Captured = { code: number; stdout: string; stderr: string }

export interface SandboxPhaseContext {
  client: Client
  cfg: TempsClientConfig
  runId: string
  workerNodeId: number
  workerName: string
  /** Outer (DinD) container names; `docker exec` into them reaches each node's own Docker daemon. */
  workerContainer: string
  controlPlaneContainer: string
  step: <T>(name: string, fn: () => Promise<T>) => Promise<T>
  skip: (name: string, reason: string) => void
  log: (msg: string) => void
  runCaptured: (args: string[]) => Promise<Captured>
  /** `docker inspect` health status of a container ('' if missing). */
  containerHealthStatus: (container: string) => Promise<string>
}

/** First create may pull (or, if the pull fails, build) the sandbox image on the worker. */
const CREATE_TIMEOUT_MS = 15 * 60_000
/**
 * A local image build (no published image) outlasts a single HTTP request,
 * so the first create waits for the image to appear on the worker and
 * retries. Bounded well inside the CI job's limit.
 */
const IMAGE_BUILD_TIMEOUT_MS = 35 * 60_000
const IMAGE_PULL_TIMEOUT_MS = 10 * 60_000
const CONTROL_PLANE_RESTART_TIMEOUT_MS = 10 * 60_000
/** 30 s heartbeats, 90 s staleness, 60 s health tick: offline within ~150 s; failover only after 300 s. */
const OFFLINE_DETECTION_TIMEOUT_MS = 240_000
const NODE_RECOVERY_TIMEOUT_MS = 180_000
const PROBE_FILE = `${SANDBOX_WORKSPACE_DIR}/temps-e2e-probe.txt`

interface DrainStatusWithSandboxes {
  status: string
  remaining_containers: number
  remaining_sandboxes?: number
  drain_complete: boolean
  can_remove: boolean
  message: string
}

export async function runMultinodeSandboxPhases(ctx: SandboxPhaseContext): Promise<void> {
  const { client, step, log, runCaptured, workerNodeId, workerName } = ctx
  const api = new SandboxApi(ctx.cfg)
  /** Sandboxes created and not yet confirmed destroyed — cleanup destroys whatever is left. */
  const live = new Set<string>()
  let workerPaused = false
  let placementTouched = false

  const dockerNames = async (container: string, all: boolean): Promise<string[]> => {
    const res = await runCaptured(['docker', 'exec', container, 'docker', 'ps', ...(all ? ['-a'] : []), '--format', '{{.Names}}'])
    if (res.code !== 0) throw new Error(`docker exec ${container} docker ps failed: ${res.stderr.trim()}`)
    return parseDockerNames(res.stdout)
  }

  /** Worker status + last heartbeat as the control plane records them (its own clock). */
  const nodeState = async (): Promise<{ status: string; lastHeartbeat: string }> => {
    try {
      const res = await adminListNodes({ client })
      if (res.error || !res.data) return { status: `(list nodes failed: HTTP ${res.response?.status ?? 0})`, lastHeartbeat: '' }
      const node = res.data.nodes.find((n) => n.id === workerNodeId)
      return { status: node?.status ?? '(missing)', lastHeartbeat: node?.last_heartbeat ?? '' }
    } catch (error) {
      return { status: `(list nodes failed: ${(error as Error).message})`, lastHeartbeat: '' }
    }
  }

  const drainStatus = async (): Promise<DrainStatusWithSandboxes> =>
    unwrap(await adminDrainStatus({ client, path: { node_id: workerNodeId } }), 'adminDrainStatus') as DrainStatusWithSandboxes

  const remainingSandboxes = (status: DrainStatusWithSandboxes): number => {
    if (typeof status.remaining_sandboxes !== 'number') {
      throw new Error(`drain status did not report remaining_sandboxes: ${JSON.stringify(status)}`)
    }
    return status.remaining_sandboxes
  }

  const setPlacement = async (allowed: number[] | null) => {
    placementTouched = true
    const placement = expectStatus(await api.setPlacement(allowed), 200, `PUT /v1/sandboxes/placement ${JSON.stringify(allowed)}`)
    if (JSON.stringify(placement.allowed_node_ids) !== JSON.stringify(allowed)) {
      throw new Error(`placement echoed allowed_node_ids=${JSON.stringify(placement.allowed_node_ids)}, expected ${JSON.stringify(allowed)}`)
    }
    return placement
  }

  const createSandbox = async (body: { name: string; node?: string }): Promise<SandboxInner> => {
    const res = await api.create(
      { ...body, timeout_secs: 3600, cpu_limit: 1, memory_limit_mb: 1024 },
      CREATE_TIMEOUT_MS,
    )
    const created = (res.json as { sandbox?: SandboxInner } | undefined)?.sandbox
    if (res.status === 201 && created?.id) live.add(created.id)
    const sandbox = expectStatus(res, 201, `POST /v1/sandboxes ${JSON.stringify(body)}`).sandbox
    if (sandbox.status !== 'running') throw new Error(`sandbox ${sandbox.id} created with status '${sandbox.status}', expected running`)
    return sandbox
  }

  /** Run `docker <args>` inside the worker's Docker-in-Docker, bounded. */
  const dockerOnWorker = async (args: string[], timeoutMs: number): Promise<{ code: number; output: string }> => {
    const proc = Bun.spawn(['docker', 'exec', ctx.workerContainer, 'docker', ...args], { stdout: 'pipe', stderr: 'pipe' })
    const timer = setTimeout(() => proc.kill(), timeoutMs)
    const [stdout, stderr, code] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text(), proc.exited])
    clearTimeout(timer)
    return { code, output: (stderr || stdout).trim() }
  }

  /**
   * The first create on a fresh worker may have to build the sandbox image,
   * which can outlast one HTTP request. When the request gets no response,
   * wait (bounded) until the image exists on the worker — the abandoned
   * create keeps building it, and the worker removes that sandbox itself
   * once its requester is gone — then create again.
   */
  const createFirstSandbox = async (body: { name: string; node?: string }, image: string): Promise<SandboxInner> => {
    const first = await api.create({ ...body, timeout_secs: 3600, cpu_limit: 1, memory_limit_mb: 1024 }, CREATE_TIMEOUT_MS)
    if (first.status !== 0 || !image) {
      const created = (first.json as { sandbox?: SandboxInner } | undefined)?.sandbox
      if (first.status === 201 && created?.id) live.add(created.id)
      const sandbox = expectStatus(first, 201, `POST /v1/sandboxes ${JSON.stringify(body)}`).sandbox
      if (sandbox.status !== 'running') throw new Error(`sandbox ${sandbox.id} created with status '${sandbox.status}', expected running`)
      return sandbox
    }
    log(`    ! first create got no response (${first.text}); waiting for ${image} to finish building on '${workerName}'`)
    await pollUntil(
      async () => (await dockerOnWorker(['image', 'inspect', '--format', '{{.Id}}', image], 30_000)).code === 0,
      (present) => present,
      { timeoutMs: IMAGE_BUILD_TIMEOUT_MS, intervalMs: 15_000, label: `${image} built on '${workerName}'` },
    )
    return createSandbox({ ...body, name: `${body.name}-retry` })
  }

  const assertOnWorker = (sandbox: SandboxInner) => {
    if (sandbox.node_id !== workerNodeId || sandbox.node_name !== workerName) {
      throw new Error(
        `sandbox ${sandbox.id} is on node_id=${sandbox.node_id} node_name='${sandbox.node_name}', expected ${workerNodeId}/'${workerName}'`,
      )
    }
  }

  const execOk = async (id: string, cmd: string[]): Promise<string> => {
    const res = await api.exec(id, cmd)
    const out = expectStatus(res, 200, `exec ${JSON.stringify(cmd)} in ${id}`)
    if (out.exit_code !== 0) {
      throw new Error(`exec ${JSON.stringify(cmd)} in ${id} exited ${out.exit_code}: ${out.stderr.trim() || out.stdout.trim()}`)
    }
    return out.stdout
  }

  /** The sandbox's work dir on the worker host, located among the agent's possible data dirs. */
  const locateWorkDir = async (id: string): Promise<string> => {
    const res = await runCaptured([
      'docker', 'exec', ctx.workerContainer, 'sh', '-c',
      'for d in "$@"; do if [ -d "$d" ]; then echo "$d"; fi; done', 'sh',
      ...workerWorkDirCandidates(id),
    ])
    const found = parseDockerNames(res.stdout)
    if (res.code !== 0 || found.length !== 1) {
      throw new Error(
        `expected exactly one work dir for ${id} on ${workerName} among ${workerWorkDirCandidates(id).join(', ')}, found [${found.join(', ')}] (exit ${res.code}: ${res.stderr.trim()})`,
      )
    }
    return found[0]!
  }

  const pathExistsOnWorker = async (path: string): Promise<boolean> => {
    const res = await runCaptured(['docker', 'exec', ctx.workerContainer, 'test', '-e', path])
    return res.code === 0
  }

  /**
   * Wait for the worker to reach `expected`. With `heartbeatAfter` (a
   * `last_heartbeat` read earlier from the same API), also require a newer
   * heartbeat, so a stale-heartbeat offline mark cannot land right after.
   */
  const waitForWorkerStatus = (expected: string, timeoutMs: number, label: string, heartbeatAfter?: string) =>
    pollUntil(
      nodeState,
      (state) => state.status === expected && (heartbeatAfter === undefined || isNewerTimestamp(state.lastHeartbeat, heartbeatAfter)),
      {
        timeoutMs,
        intervalMs: 5000,
        onPoll: (state, elapsed) =>
          log(`    ...${workerName} status=${state.status} last_heartbeat=${state.lastHeartbeat || '-'} (${Math.round(elapsed / 1000)}s)`),
        label,
      },
    )

  const unpauseWorker = async () => {
    const res = await runCaptured(['docker', 'unpause', ctx.workerContainer])
    if (res.code !== 0 && !res.stderr.includes('is not paused')) {
      throw new Error(`docker unpause ${ctx.workerContainer} failed: ${res.stderr.trim()}`)
    }
    workerPaused = false
  }

  const body = async () => {
    await step(`reactivate the drained '${workerName}' so it can take sandboxes (DELETE /internal/nodes/{id}/drain)`, async () => {
      unwrap(await adminUndrainNode({ client, path: { node_id: workerNodeId } }), 'adminUndrainNode')
      await waitForWorkerStatus('active', 60_000, `${workerName} to report active after undrain`)
    })

    await step(`placement: GET /v1/sandboxes/placement lists '${workerName}' as eligible`, async () => {
      const placement = await pollUntil(
        async () => api.placement(),
        (res) => {
          if (res.status !== 200) return false
          const worker = findPlacementNode(res.json as SandboxPlacement, workerNodeId)
          return worker?.eligible === true
        },
        {
          timeoutMs: 120_000,
          intervalMs: 5000,
          onPoll: (res) => {
            const worker = res.status === 200 ? findPlacementNode(res.json as SandboxPlacement, workerNodeId) : undefined
            log(`    ...${worker ? `eligible=${worker.eligible} allowed=${worker.allowed} status=${worker.status} reason=${worker.reason ?? '-'}` : describeResult(res)}`)
          },
          label: `${workerName} to be listed as an eligible sandbox node`,
        },
      )
      const state = placement.json as SandboxPlacement
      if (state.allowed_node_ids !== null) {
        throw new Error(`fresh cluster should default to allowed_node_ids=null (every node), got ${JSON.stringify(state.allowed_node_ids)}`)
      }
      if (!state.nodes.some((n) => n.is_control_plane && n.id === 0)) {
        throw new Error(`placement does not list the control plane as node 0: ${placement.text}`)
      }
    })

    await step(`placement: PUT allowed_node_ids [${workerNodeId}] then back to null`, async () => {
      const restricted = await setPlacement([workerNodeId])
      const worker = findPlacementNode(restricted, workerNodeId)
      const controlPlane = findPlacementNode(restricted, 0)
      if (!worker?.allowed) throw new Error(`${workerName} not allowed after PUT [${workerNodeId}]: ${JSON.stringify(worker)}`)
      if (controlPlane?.allowed !== false) throw new Error(`control plane still allowed after PUT [${workerNodeId}]: ${JSON.stringify(controlPlane)}`)
      const reset = await setPlacement(null)
      const notAllowed = reset.nodes.filter((n) => !n.allowed)
      if (notAllowed.length) throw new Error(`allowed_node_ids=null left nodes disallowed: ${notAllowed.map((n) => n.name).join(', ')}`)
    })

    const imageName = await step('resolve the sandbox image the control plane hands to workers', async () => {
      const res = await getGlobalSandboxStatus({ client })
      const name = res.data?.image_name ?? ''
      if (!name) log(`    ! control plane reported no sandbox image (${JSON.stringify(res.data ?? res.error)})`)
      return name
    })
    if (imageName) {
      await step(`pre-pull ${imageName} on '${workerName}' (best effort, bounded ${IMAGE_PULL_TIMEOUT_MS / 60_000} min)`, async () => {
        const pull = await dockerOnWorker(['pull', '--quiet', imageName], IMAGE_PULL_TIMEOUT_MS)
        if (pull.code === 0) return
        log(`    ! pull failed (exit ${pull.code}): ${pull.output.slice(0, 300)}`)
        // An unpublished version tag (a build without a release manifest):
        // use the published beta image under the exact name the control
        // plane asks for, instead of a 15+ minute local build.
        for (const candidate of sandboxImageFallbacks(imageName)) {
          const fallback = await dockerOnWorker(['pull', '--quiet', candidate], IMAGE_PULL_TIMEOUT_MS)
          if (fallback.code !== 0) {
            log(`    ! fallback ${candidate} did not pull either (exit ${fallback.code})`)
            continue
          }
          const tagged = await dockerOnWorker(['tag', candidate, imageName], 60_000)
          if (tagged.code === 0) {
            log(`    using ${candidate}, tagged as ${imageName}`)
            return
          }
          log(`    ! could not tag ${candidate} as ${imageName}: ${tagged.output.slice(0, 200)}`)
        }
        // Not fatal: the provider builds the image during the first create,
        // which createFirstSandbox waits for.
        log('    ! no published image; the first create builds it on the worker')
      })
    } else {
      ctx.skip(`pre-pull the sandbox image on '${workerName}'`, 'control plane reported no image name; the first create provisions it')
    }

    const primary = await step(`create a sandbox on '${workerName}' (node: "${workerName}")`, async () => {
      const sandbox = await createFirstSandbox({ name: `${ctx.runId}-sbx-worker`, node: workerName }, imageName)
      assertOnWorker(sandbox)
      return sandbox
    })
    const primaryContainer = sandboxContainerName(primary.id)
    log(`  sandbox ${primary.id} -> ${primaryContainer} on ${workerName}`)

    const workDir = await step(`assert ${primaryContainer} runs on '${workerName}' Docker (work dir on its host) and NOT on the control plane's`, async () => {
      await pollUntil(
        () => dockerNames(ctx.workerContainer, false),
        (names) => names.includes(primaryContainer),
        { timeoutMs: 30_000, intervalMs: 1000, label: `${primaryContainer} to be running on ${workerName}` },
      )
      const onControlPlane = sandboxContainersIn(await dockerNames(ctx.controlPlaneContainer, true), primary.id)
      if (onControlPlane.length) throw new Error(`sandbox containers found on the control plane: ${onControlPlane.join(', ')}`)
      return locateWorkDir(primary.id)
    })
    log(`  work dir on ${workerName}: ${workDir}`)

    await step(`exec echo + hostname runs inside ${primaryContainer}`, async () => {
      const marker = `temps-e2e-${ctx.runId}`
      const { markerSeen, hostname } = parseMarkerAndHostname(
        await execOk(primary.id, ['sh', '-c', `echo ${marker}; hostname 2>/dev/null || cat /proc/sys/kernel/hostname`]),
        marker,
      )
      if (!markerSeen) throw new Error(`exec output did not contain the echo marker '${marker}'`)
      const inspect = await runCaptured(['docker', 'exec', ctx.workerContainer, 'docker', 'inspect', '-f', '{{.Config.Hostname}}', primaryContainer])
      const expected = inspect.stdout.trim()
      if (inspect.code !== 0 || !expected) throw new Error(`docker inspect ${primaryContainer} on ${workerName} failed: ${inspect.stderr.trim()}`)
      if (hostname !== expected) throw new Error(`exec hostname '${hostname}' != ${workerName}'s container hostname '${expected}'`)
    })

    const probe = `written by ${ctx.runId} at ${new Date().toISOString()}\n`
    await step('write a file, read it back, and find it in the worker work dir', async () => {
      expectStatus(await api.writeFile(primary.id, PROBE_FILE, probe), 204, `write ${PROBE_FILE}`)
      const read = expectStatus(await api.readFile(primary.id, PROBE_FILE), 200, `read ${PROBE_FILE}`)
      const contents = fromBase64(read.contents_b64)
      if (contents !== probe) throw new Error(`read back ${JSON.stringify(contents)}, wrote ${JSON.stringify(probe)}`)
      const onHost = await runCaptured(['docker', 'exec', ctx.workerContainer, 'cat', `${workDir}/temps-e2e-probe.txt`])
      if (onHost.code !== 0 || onHost.stdout !== probe) {
        throw new Error(`file not found with the same contents in ${workDir} on ${workerName}: ${onHost.stderr.trim() || JSON.stringify(onHost.stdout)}`)
      }
    })

    await step('stop (pause) then start (resume); exec works and the file survived', async () => {
      const paused = expectStatus(await api.pause(primary.id), 200, 'pause').sandbox
      if (paused.status !== 'stopped') throw new Error(`pause returned status '${paused.status}', expected stopped`)
      const state = await runCaptured(['docker', 'exec', ctx.workerContainer, 'docker', 'inspect', '-f', '{{.State.Running}}', primaryContainer])
      if (state.stdout.trim() !== 'false') throw new Error(`${primaryContainer} still running on ${workerName} after pause (${state.stdout.trim() || state.stderr.trim()})`)
      const resumed = expectStatus(await api.resume(primary.id), 200, 'resume').sandbox
      if (resumed.status !== 'running') throw new Error(`resume returned status '${resumed.status}', expected running`)
      const contents = await execOk(primary.id, ['cat', PROBE_FILE])
      if (contents !== probe) throw new Error(`after resume the probe file reads ${JSON.stringify(contents)}`)
    })

    await step(`restart the control plane; exec on the worker sandbox recovers (bounded ${CONTROL_PLANE_RESTART_TIMEOUT_MS / 60_000} min)`, async () => {
      const before = (await nodeState()).lastHeartbeat
      const restart = await runCaptured(['docker', 'restart', '-t', '30', ctx.controlPlaneContainer])
      if (restart.code !== 0) throw new Error(`docker restart ${ctx.controlPlaneContainer} failed: ${restart.stderr.trim()}`)
      await pollUntil(
        () => ctx.containerHealthStatus(ctx.controlPlaneContainer),
        (status) => status === 'healthy',
        {
          timeoutMs: CONTROL_PLANE_RESTART_TIMEOUT_MS,
          intervalMs: 5000,
          onPoll: (status) => log(`    ...control-plane health=${status || '(missing)'}`),
          label: 'restarted control plane to report healthy',
        },
      )
      await waitForWorkerStatus(
        'active',
        NODE_RECOVERY_TIMEOUT_MS,
        `control-plane API to answer and ${workerName} to heartbeat the restarted control plane`,
        before,
      )
      const marker = `after-restart-${ctx.runId}`
      const out = await pollUntil(
        () => api.exec(primary.id, ['echo', marker]),
        (res) => res.status === 200,
        { timeoutMs: 60_000, intervalMs: 3000, onPoll: (res) => log(`    ...exec ${describeResult(res)}`), label: 'exec after control-plane restart' },
      )
      const result = out.json as { exit_code: number; stdout: string }
      if (result.exit_code !== 0 || !result.stdout.includes(marker)) throw new Error(`exec after restart: ${out.text}`)
      assertOnWorker(expectStatus(await api.get(primary.id), 200, 'GET sandbox after restart').sandbox)
    })

    await step('snapshot of a worker sandbox is refused with 422 and the sandbox keeps running', async () => {
      expectProblem(
        await api.snapshot(primary.id, `${ctx.runId}-snap`),
        // The worker-feature refusal is being unified under one type; accept
        // the snapshot-specific type the first implementation returns too.
        { status: 422, types: [SANDBOX_PROBLEM_TYPES.snapshotOnWorkerNode, SANDBOX_PROBLEM_TYPES.unsupportedOnWorkerNode] },
        'POST /snapshots on a worker sandbox',
      )
      const after = expectStatus(await api.get(primary.id), 200, 'GET sandbox after refused snapshot').sandbox
      if (after.status !== 'running') throw new Error(`sandbox status '${after.status}' after a refused snapshot, expected running`)
      await execOk(primary.id, ['true'])
      if (!(await dockerNames(ctx.workerContainer, false)).includes(primaryContainer)) {
        throw new Error(`${primaryContainer} no longer running on ${workerName} after a refused snapshot`)
      }
    })

    await step(`disallowed node: allowed_node_ids [0] + node "${workerName}" -> 422 sandbox-node-not-allowed`, async () => {
      await setPlacement([0])
      const res = await api.create({ name: `${ctx.runId}-sbx-denied`, node: workerName, timeout_secs: 600 }, CREATE_TIMEOUT_MS)
      const created = (res.json as { sandbox?: SandboxInner } | undefined)?.sandbox
      if (res.status === 201 && created?.id) live.add(created.id)
      expectProblem(res, { status: 422, types: [SANDBOX_PROBLEM_TYPES.nodeNotAllowed], detailIncludes: workerName }, 'create on a disallowed node')
    })

    const secondary = await step(`default placement with the control plane excluded (allowed [${workerNodeId}], no node) lands on '${workerName}'`, async () => {
      await setPlacement([workerNodeId])
      const sandbox = await createSandbox({ name: `${ctx.runId}-sbx-default` })
      assertOnWorker(sandbox)
      await setPlacement(null)
      return sandbox
    })
    const secondaryWorkDir = await step(`second sandbox runs on '${workerName}' with its own work dir`, async () => {
      await pollUntil(
        () => dockerNames(ctx.workerContainer, false),
        (names) => names.includes(sandboxContainerName(secondary.id)),
        { timeoutMs: 30_000, intervalMs: 1000, label: `${sandboxContainerName(secondary.id)} to be running on ${workerName}` },
      )
      return locateWorkDir(secondary.id)
    })

    await step(`offline node: freeze '${workerName}' (docker pause) -> exec returns 503 naming it, then it recovers`, async () => {
      let frozenHeartbeat = ''
      const pause = await runCaptured(['docker', 'pause', ctx.workerContainer])
      if (pause.code !== 0) throw new Error(`docker pause ${ctx.workerContainer} failed: ${pause.stderr.trim()}`)
      workerPaused = true
      try {
        await waitForWorkerStatus('offline', OFFLINE_DETECTION_TIMEOUT_MS, `${workerName} to be marked offline while frozen`)
        const t0 = performance.now()
        const res = await api.exec(primary.id, ['true'], 60_000)
        const seconds = ((performance.now() - t0) / 1000).toFixed(1)
        expectProblem(res, { status: 503, detailIncludes: workerName }, `exec on a sandbox whose node is offline (answered in ${seconds}s)`)
        log(`    exec on the offline node answered 503 in ${seconds}s: ${parseProblem(res.json)?.detail ?? ''}`)
      } finally {
        frozenHeartbeat = (await nodeState()).lastHeartbeat
        await unpauseWorker()
      }
      await waitForWorkerStatus('active', NODE_RECOVERY_TIMEOUT_MS, `${workerName} to reconnect after unpause`, frozenHeartbeat)
      await pollUntil(
        () => api.exec(primary.id, ['true']),
        (res) => res.status === 200 && (res.json as { exit_code?: number }).exit_code === 0,
        { timeoutMs: 60_000, intervalMs: 3000, onPoll: (res) => log(`    ...exec ${describeResult(res)}`), label: 'exec after the worker came back' },
      )
    })

    await step(`node removal refused while sandboxes live on '${workerName}' (DELETE /internal/nodes/{id} -> 409)`, async () => {
      const res = await adminRemoveNode({ client, path: { node_id: workerNodeId } })
      const status = res.response?.status ?? 0
      if (status !== 409) throw new Error(`expected 409, got HTTP ${status}: ${JSON.stringify(res.error ?? res.data)}`)
      // The handler refuses for deployment containers first ("Node Has Active
      // Containers"); only the sandbox refusal mentions sandboxes.
      const problem = parseProblem(res.error)
      const text = problem ? `${problem.title ?? ''}: ${problem.detail ?? ''}` : String(res.error ?? '')
      if (!/sandbox/i.test(text)) {
        throw new Error(`409 was not about sandboxes (the worker should host no deployment containers here): ${text}`)
      }
      const drain = await drainStatus()
      if (remainingSandboxes(drain) < 2) throw new Error(`drain status remaining_sandboxes=${drain.remaining_sandboxes}, expected >= 2`)
      if (drain.can_remove) throw new Error(`drain status says can_remove while sandboxes remain: ${drain.message}`)
    })

    await step(`destroy the second sandbox: its container and its work dir on '${workerName}' are gone`, async () => {
      expectStatus(await api.destroy(secondary.id), 204, `destroy ${secondary.id}`)
      live.delete(secondary.id)
      const leftovers = await pollUntil(
        async () => sandboxContainersIn(await dockerNames(ctx.workerContainer, true), secondary.id),
        (names) => names.length === 0,
        { timeoutMs: 30_000, intervalMs: 1000, onPoll: (names) => log(`    ...left: ${names.join(', ') || '(none)'}`), label: `${secondary.id}'s containers to be removed` },
      )
      if (leftovers.length) throw new Error(`containers left: ${leftovers.join(', ')}`)
      if (await pathExistsOnWorker(secondaryWorkDir)) throw new Error(`work dir ${secondaryWorkDir} still exists on ${workerName}`)
    })

    await step(`evict '${workerName}': every sandbox destroyed, containers confirmed gone, removal no longer blocked`, async () => {
      const resp = expectStatus(await api.evict(workerName, 180_000), 200, `evict ${workerName}`)
      const issues = evictionIssues(resp, [primary.id])
      if (issues.length) throw new Error(issues.join('; '))
      live.delete(primary.id)
      await pollUntil(
        async () => sandboxContainersIn(await dockerNames(ctx.workerContainer, true)),
        (names) => names.length === 0,
        { timeoutMs: 30_000, intervalMs: 1000, onPoll: (names) => log(`    ...left: ${names.join(', ') || '(none)'}`), label: `every temps-sandbox- container to be gone from ${workerName}` },
      )
      if (await pathExistsOnWorker(workDir)) throw new Error(`work dir ${workDir} still exists on ${workerName} after eviction`)
      const drain = await drainStatus()
      if (remainingSandboxes(drain) !== 0) throw new Error(`drain status remaining_sandboxes=${drain.remaining_sandboxes} after eviction`)
    })

    await step(`drain '${workerName}' again: drain status allows removal with no sandboxes left`, async () => {
      unwrap(await adminDrainNode({ client, path: { node_id: workerNodeId } }), 'adminDrainNode')
      const drain = await pollUntil(drainStatus, (s) => s.drain_complete && s.can_remove, {
        timeoutMs: 120_000,
        intervalMs: 3000,
        onPoll: (s) => log(`    ...drain_complete=${s.drain_complete} can_remove=${s.can_remove} containers=${s.remaining_containers} sandboxes=${s.remaining_sandboxes}`),
        label: `${workerName} drain to allow removal`,
      })
      if (remainingSandboxes(drain) !== 0) throw new Error(`remaining_sandboxes=${drain.remaining_sandboxes}`)
    })
  }

  /** Best effort; returns what could not be cleaned up. Never throws. */
  const cleanup = async (): Promise<string[]> => {
    const errors: string[] = []
    if (workerPaused) {
      await unpauseWorker().catch((e: Error) => errors.push(e.message))
    }
    for (const id of live) {
      // A worker that was just unpaused may need a heartbeat before the
      // control plane routes to it again; retry briefly.
      const res = await pollUntil(
        () => api.destroy(id, 60_000),
        (r) => r.status === 204 || r.status === 404,
        { timeoutMs: 60_000, intervalMs: 5000, label: `destroy leftover sandbox ${id}` },
      ).catch((e: Error) => ({ status: -1, text: e.message, json: undefined }))
      if (res.status === 204 || res.status === 404) live.delete(id)
      else errors.push(`destroy ${id}: ${describeResult(res)}`)
    }
    if (placementTouched) {
      const res = await api.setPlacement(null)
      const allowed = (res.json as { allowed_node_ids?: unknown } | undefined)?.allowed_node_ids
      if (res.status !== 200 || allowed !== null) errors.push(`restore allowed_node_ids=null: ${describeResult(res)}`)
    }
    return errors
  }

  let failure: unknown
  try {
    await body()
  } catch (error) {
    failure = error
  }
  if (failure !== undefined) {
    const errors = await cleanup()
    log(`  sandbox cleanup after failure: ${errors.length ? errors.join('; ') : 'ok (sandboxes destroyed, allow-list restored to null)'}`)
    throw failure
  }
  await step('sandbox cleanup: nothing left behind, allowed_node_ids restored to null', async () => {
    const errors = await cleanup()
    if (errors.length) throw new Error(errors.join('; '))
  })
}
