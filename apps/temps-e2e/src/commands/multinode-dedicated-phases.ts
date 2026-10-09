// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Dedicated nodes (`temps.sh/role=dedicated`), run inside
 * `multinode-join-scenario` against its real 2-node cluster.
 *
 * Runs after the sandbox phases and before the scenario removes the worker,
 * when the worker is drained and hosts nothing: restarting its agent to pick
 * up a new label cannot disturb anything the outer scenario still asserts
 * on.
 *
 * Phases (each a PASS/FAIL step line in the scenario's output):
 *   1. add `temps.sh/role=dedicated` to the worker's `agent.json` labels and
 *      restart it; the control plane reports the label after a fresh
 *      heartbeat (the agent sends its labels on every beat)
 *   2. reactivate the drained worker
 *   3. an UNPINNED 2-replica deployment with anti-affinity lands entirely on
 *      the control plane: without the dedicated role, anti-affinity would put
 *      one replica on the worker. The deploy log names the skipped node, and
 *      the dedicated exclusion does not turn into an anti-affinity shortfall.
 *   4. a SELECTOR-ONLY deployment (`target_labels` matching the worker's
 *      labels, no `target_nodes`) fails with the dedicated-node error instead
 *      of landing on the worker
 *   5. the same environment PINNED to the worker (`target_nodes`, selector
 *      kept) lands on the worker's own Docker, not the control plane's
 *   6. sandbox placement lists the worker as dedicated; with the allow-list
 *      restricted to it, a sandbox created without `node` is refused with
 *      422 sandbox-dedicated-node-only
 *   7. tear the applications down and drain the worker again so the outer
 *      scenario can remove it
 * Cleanup (always): restore the sandbox allow-list to null, tear down the
 * applications this phase created.
 */
import {
  adminDrainNode,
  adminDrainStatus,
  adminListNodes,
  adminUndrainNode,
  deployFromImage,
  getDeployment,
  getDeploymentJobLogs,
  getDeploymentJobs,
  updateEnvironmentSettings,
} from '@temps-sdk/api'
import type { Client } from '@temps-sdk/api/client'
import { unwrap, type TempsClientConfig } from '../lib/client.ts'
import {
  createE2eProject,
  getProductionEnvironment,
  pollUntil,
  teardown,
  waitForDeployment,
} from '../lib/flows.ts'
import {
  SANDBOX_PROBLEM_TYPES,
  SandboxApi,
  describeResult,
  expectProblem,
  expectStatus,
  findPlacementNode,
  isNewerTimestamp,
  type SandboxInner,
  type SandboxPlacement,
} from '../lib/sandbox-api.ts'

type Captured = { code: number; stdout: string; stderr: string }

export const NODE_ROLE_LABEL = 'temps.sh/role'
export const DEDICATED_NODE_ROLE = 'dedicated'

export interface DedicatedPhaseContext {
  client: Client
  cfg: TempsClientConfig
  runId: string
  workerNodeId: number
  workerName: string
  /** Outer (DinD) container names; `docker exec` into them reaches each node's own Docker daemon. */
  workerContainer: string
  controlPlaneContainer: string
  step: <T>(name: string, fn: () => Promise<T>) => Promise<T>
  log: (msg: string) => void
  runCaptured: (args: string[]) => Promise<Captured>
  /** Container names on a node's own Docker daemon. */
  dockerPsNames: (container: string) => Promise<string[]>
}

/**
 * The worker restarts its agent (and, on a source-built cluster, re-checks
 * its binary) before the next heartbeat carries the new label.
 */
const RELABEL_TIMEOUT_MS = 15 * 60_000
const DEPLOY_TIMEOUT_MS = 5 * 60_000
const DEDICATED_EXCLUSION = `is dedicated (${NODE_ROLE_LABEL}=${DEDICATED_NODE_ROLE})`

/** jq program that adds the dedicated role to an agent config, keeping every other label. */
export function dedicatedLabelJq(): string {
  return `.labels = ((.labels // {}) + {"${NODE_ROLE_LABEL}": "${DEDICATED_NODE_ROLE}"})`
}

/** Whether a node's labels (as returned by GET /internal/nodes) carry the dedicated role. */
export function hasDedicatedRole(labels: unknown): boolean {
  if (!labels || typeof labels !== 'object' || Array.isArray(labels)) return false
  return (labels as Record<string, unknown>)[NODE_ROLE_LABEL] === DEDICATED_NODE_ROLE
}

/** Containers on a node that belong to `projectSlug`. */
export function projectContainers(names: readonly string[], projectSlug: string): string[] {
  return names.filter((name) => name.includes(projectSlug))
}

export async function runMultinodeDedicatedPhases(ctx: DedicatedPhaseContext): Promise<void> {
  const { client, step, log, runCaptured, workerNodeId, workerName, dockerPsNames } = ctx
  const api = new SandboxApi(ctx.cfg)
  const projectIds: number[] = []
  const deployments: { projectId: number; deploymentId: number }[] = []
  const straySandboxes: string[] = []
  let placementTouched = false

  const workerNode = async () => {
    const res = await adminListNodes({ client })
    if (res.error || !res.data) return undefined
    return res.data.nodes.find((node) => node.id === workerNodeId)
  }

  /** Every job's error, log and the deployment's own reason, for asserting on what the user sees. */
  const deploymentText = async (projectId: number, deploymentId: number): Promise<string> => {
    const parts: string[] = []
    const deployment = unwrap(
      await getDeployment({ client, path: { project_id: projectId, deployment_id: deploymentId } }),
      `getDeployment(${deploymentId})`,
    )
    if (deployment.cancelled_reason) parts.push(deployment.cancelled_reason)
    const jobs = unwrap(
      await getDeploymentJobs({ client, path: { project_id: projectId, deployment_id: deploymentId } }),
      `getDeploymentJobs(${deploymentId})`,
    ).jobs
    for (const job of jobs) {
      if (job.error_message) parts.push(job.error_message)
      const logs = await getDeploymentJobLogs({
        client,
        path: { project_id: projectId, deployment_id: deploymentId, job_id: job.job_id },
        parseAs: 'text',
      })
      if (typeof logs.data === 'string') parts.push(logs.data)
    }
    return parts.join('\n')
  }

  const deploy = async (projectId: number, environmentId: number): Promise<number> => {
    const deploymentId = unwrap(
      await deployFromImage({
        client,
        path: { project_id: projectId, environment_id: environmentId },
        body: { image_ref: 'traefik/whoami:latest' },
      }),
      'deployFromImage',
    ).id
    deployments.push({ projectId, deploymentId })
    return deploymentId
  }

  /** Poll both nodes' Docker until `predicate` holds for this project's containers. */
  const waitForPlacement = (
    projectSlug: string,
    predicate: (placement: { worker: string[]; controlPlane: string[] }) => boolean,
    label: string,
  ) =>
    pollUntil(
      async () => {
        const [worker, controlPlane] = await Promise.all([
          dockerPsNames(ctx.workerContainer),
          dockerPsNames(ctx.controlPlaneContainer),
        ])
        return {
          worker: projectContainers(worker, projectSlug),
          controlPlane: projectContainers(controlPlane, projectSlug),
        }
      },
      predicate,
      {
        timeoutMs: 60_000,
        intervalMs: 1000,
        onPoll: (placement) =>
          log(`    ...worker=[${placement.worker.join(', ')}] control-plane=[${placement.controlPlane.join(', ')}]`),
        label,
      },
    )

  const body = async () => {
    await step(
      `mark '${workerName}' dedicated: add ${NODE_ROLE_LABEL}=${DEDICATED_NODE_ROLE} to its agent.json labels and restart its agent`,
      async () => {
        const before = (await workerNode())?.last_heartbeat ?? ''
        const edit = await runCaptured([
          'docker',
          'exec',
          ctx.workerContainer,
          'sh',
          '-ec',
          // Rewrite in place (cat >) so the file keeps its owner and 0600 mode.
          `jq '${dedicatedLabelJq()}' /root/.temps/agent.json > /root/.temps/agent.json.dedicated && cat /root/.temps/agent.json.dedicated > /root/.temps/agent.json && rm /root/.temps/agent.json.dedicated`,
        ])
        if (edit.code !== 0) {
          throw new Error(`could not add the dedicated label to ${workerName}'s agent.json: ${edit.stderr.trim() || edit.stdout.trim()}`)
        }
        const restart = await runCaptured(['docker', 'restart', '-t', '20', ctx.workerContainer])
        if (restart.code !== 0) {
          throw new Error(`docker restart ${ctx.workerContainer} failed: ${restart.stderr.trim()}`)
        }
        await pollUntil(
          workerNode,
          (node) =>
            node !== undefined &&
            hasDedicatedRole(node.labels) &&
            (before === '' || isNewerTimestamp(node.last_heartbeat ?? '', before)),
          {
            timeoutMs: RELABEL_TIMEOUT_MS,
            intervalMs: 5000,
            onPoll: (node, elapsed) =>
              log(
                `    ...${workerName} status=${node?.status ?? '(missing)'} labels=${JSON.stringify(node?.labels ?? null)} (${Math.round(elapsed / 1000)}s)`,
              ),
            label: `${workerName} to heartbeat with ${NODE_ROLE_LABEL}=${DEDICATED_NODE_ROLE}`,
          },
        )
      },
    )

    await step(`reactivate the drained, now dedicated '${workerName}'`, async () => {
      const undrain = await adminUndrainNode({ client, path: { node_id: workerNodeId } })
      if (undrain.error && (await workerNode())?.status !== 'active') {
        throw new Error(`undrain of ${workerName} failed: HTTP ${undrain.response?.status ?? 0}`)
      }
      await pollUntil(workerNode, (node) => node?.status === 'active', {
        timeoutMs: 120_000,
        intervalMs: 3000,
        onPoll: (node) => log(`    ...${workerName} status=${node?.status ?? '(missing)'}`),
        label: `${workerName} to report active after undrain`,
      })
    })

    const unpinned = await step('create an unpinned application with 2 replicas and anti-affinity', async () => {
      const project = await createE2eProject(client, { name: `${ctx.runId}-dn-free`, exposedPort: 80 })
      projectIds.push(project.id)
      const env = await getProductionEnvironment(client, project.id)
      unwrap(
        await updateEnvironmentSettings({
          client,
          path: { project_id: project.id, env_id: env.id },
          body: { replicas: 2, anti_affinity: true, target_nodes: [] },
        }),
        'updateEnvironmentSettings',
      )
      return { project, env }
    })

    await step(`unpinned deployment: every replica avoids the dedicated '${workerName}'`, async () => {
      const deploymentId = await deploy(unpinned.project.id, unpinned.env.id)
      const status = await waitForDeployment(client, {
        projectId: unpinned.project.id,
        deploymentId,
        timeoutMs: DEPLOY_TIMEOUT_MS,
        onPoll: (s) => log(`    ...${s.state}`),
      })
      if (!status.ok) {
        const text = await deploymentText(unpinned.project.id, deploymentId)
        throw new Error(`unpinned deployment ${deploymentId} ended "${status.state}": ${text.slice(-1500)}`)
      }
      const placement = await waitForPlacement(
        unpinned.project.slug,
        (p) => p.controlPlane.length >= 2,
        'both replicas to appear on the control plane',
      )
      if (placement.worker.length > 0) {
        throw new Error(
          `unpinned deployment placed ${placement.worker.length} replica(s) on the dedicated worker: [${placement.worker.join(', ')}]`,
        )
      }
      const text = await deploymentText(unpinned.project.id, deploymentId)
      if (!text.includes(`'${workerName}' ${DEDICATED_EXCLUSION}`)) {
        throw new Error(`deploy log does not say '${workerName}' was skipped as dedicated:\n${text.slice(-1500)}`)
      }
      log(`  control-plane=[${placement.controlPlane.join(', ')}]; deploy log names the skipped dedicated node`)
    })

    const selected = await step(
      `create an application whose label selector matches '${workerName}' (${NODE_ROLE_LABEL}=${DEDICATED_NODE_ROLE}), not pinned`,
      async () => {
        const project = await createE2eProject(client, { name: `${ctx.runId}-dn-pin`, exposedPort: 80 })
        projectIds.push(project.id)
        const env = await getProductionEnvironment(client, project.id)
        unwrap(
          await updateEnvironmentSettings({
            client,
            path: { project_id: project.id, env_id: env.id },
            body: { target_labels: { [NODE_ROLE_LABEL]: DEDICATED_NODE_ROLE }, target_nodes: [] },
          }),
          'updateEnvironmentSettings',
        )
        return { project, env }
      },
    )

    await step('selector-only deployment fails with the dedicated-node error instead of landing on the worker', async () => {
      const deploymentId = await deploy(selected.project.id, selected.env.id)
      const status = await waitForDeployment(client, {
        projectId: selected.project.id,
        deploymentId,
        timeoutMs: DEPLOY_TIMEOUT_MS,
        onPoll: (s) => log(`    ...${s.state}`),
      })
      if (status.ok) {
        throw new Error(`selector-only deployment ${deploymentId} succeeded; a label selector must not select a dedicated node`)
      }
      const text = await deploymentText(selected.project.id, deploymentId)
      const expected = `node ${workerNodeId} (${workerName}) ${DEDICATED_EXCLUSION}`
      if (!text.includes('No eligible node for this deployment') || !text.includes(expected)) {
        throw new Error(`deployment ${deploymentId} failed ("${status.state}") without the dedicated-node error naming "${expected}":\n${text.slice(-1500)}`)
      }
      const worker = projectContainers(await dockerPsNames(ctx.workerContainer), selected.project.slug)
      if (worker.length > 0) throw new Error(`a container still landed on the worker: [${worker.join(', ')}]`)
      log(`  failed as expected: ...${expected}...`)
    })

    await step(`pin the same environment to '${workerName}' (target_nodes=[${workerNodeId}], selector kept)`, async () => {
      unwrap(
        await updateEnvironmentSettings({
          client,
          path: { project_id: selected.project.id, env_id: selected.env.id },
          body: { target_nodes: [workerNodeId] },
        }),
        'updateEnvironmentSettings',
      )
    })

    await step(`pinned deployment lands on the dedicated '${workerName}', not the control plane`, async () => {
      const deploymentId = await deploy(selected.project.id, selected.env.id)
      const status = await waitForDeployment(client, {
        projectId: selected.project.id,
        deploymentId,
        timeoutMs: DEPLOY_TIMEOUT_MS,
        onPoll: (s) => log(`    ...${s.state}`),
      })
      if (!status.ok) {
        const text = await deploymentText(selected.project.id, deploymentId)
        throw new Error(`pinned deployment ${deploymentId} ended "${status.state}": ${text.slice(-1500)}`)
      }
      const placement = await waitForPlacement(
        selected.project.slug,
        (p) => p.worker.length >= 1,
        `the pinned container to appear on ${workerName}`,
      )
      if (placement.controlPlane.length > 0) {
        throw new Error(`pinned deployment also has container(s) on the control plane: [${placement.controlPlane.join(', ')}]`)
      }
      log(`  ${workerName}=[${placement.worker.join(', ')}]`)
    })

    await step(`sandboxes: placement flags '${workerName}' dedicated; automatic placement onto it is refused`, async () => {
      placementTouched = true
      expectStatus(await api.setPlacement([workerNodeId]), 200, `PUT /v1/sandboxes/placement [${workerNodeId}]`)
      const placement = expectStatus(await api.placement(), 200, 'GET /v1/sandboxes/placement') as SandboxPlacement
      const listed = findPlacementNode(placement, workerNodeId)
      if (!listed?.dedicated || !listed.eligible) {
        throw new Error(`placement lists ${workerName} as ${JSON.stringify(listed)}, expected dedicated and eligible`)
      }
      const res = await api.create({ name: `${ctx.runId}-dn-auto`, timeout_secs: 600, cpu_limit: 1, memory_limit_mb: 512 }, 120_000)
      const created = (res.json as { sandbox?: SandboxInner } | undefined)?.sandbox
      if (res.status === 201 && created?.id) straySandboxes.push(created.id)
      expectProblem(
        res,
        { status: 422, types: [SANDBOX_PROBLEM_TYPES.dedicatedNodeOnly], detailIncludes: DEDICATED_EXCLUSION },
        'POST /v1/sandboxes with no node while only a dedicated worker is allowed',
      )
    })

    await step(`tear the dedicated-phase applications down and drain '${workerName}' again for removal`, async () => {
      const cleanup = await teardown(client, { deployments: deployments.splice(0), projectIds: projectIds.splice(0) })
      if (cleanup.errors.length) throw new Error(cleanup.errors.join('; '))
      await waitForPlacement(selected.project.slug, (p) => p.worker.length === 0, `the pinned container to leave ${workerName}`)
      unwrap(await adminDrainNode({ client, path: { node_id: workerNodeId } }), 'adminDrainNode')
      await pollUntil(
        async () => unwrap(await adminDrainStatus({ client, path: { node_id: workerNodeId } }), 'adminDrainStatus'),
        (status) => status.drain_complete,
        {
          timeoutMs: 180_000,
          intervalMs: 3000,
          onPoll: (status) => log(`    ...drain_complete=${status.drain_complete} remaining=${status.remaining_containers}`),
          label: `${workerName} drain to complete`,
        },
      )
    })
  }

  try {
    await body()
  } finally {
    for (const id of straySandboxes) {
      const res = await api.destroy(id, 120_000)
      if (res.status !== 200 && res.status !== 204) log(`    ! cleanup: destroy stray sandbox ${id}: ${describeResult(res)}`)
    }
    if (placementTouched) {
      const res = await api.setPlacement(null)
      if (res.status !== 200) log(`    ! cleanup: restore sandbox allow-list to null: ${describeResult(res)}`)
    }
    if (deployments.length || projectIds.length) {
      const cleanup = await teardown(client, { deployments, projectIds })
      for (const error of cleanup.errors) log(`    ! cleanup: ${error}`)
    }
  }
}
