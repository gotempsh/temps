// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Operator actions on one worker node: inspect it and its containers, drain,
 * undrain and remove it, its build limits and public ingress, and an
 * environment's workload mode.
 */

import type { Command } from 'commander'
import { client } from '../../lib/api-client.js'
import {
  adminCheckNodePublicIngress,
  adminDrainNode,
  adminDrainStatus,
  adminGetEnvironmentWorkloadMode,
  adminGetNode,
  adminGetNodeBuildLimits,
  adminGetNodePublicIngressSettings,
  adminListNodeContainers,
  adminRemoveNode,
  adminSetEnvironmentWorkloadMode,
  adminSetNodeBuildLimits,
  adminSetNodePublicIngress,
  adminUndrainNode,
  adminUpdateNodePublicIngressSettings,
} from '../../api/sdk.gen.js'
import type { BuildLimits, EnvironmentWorkloadMode, UpdatePublicIngressSettingsRequest } from '../../api/types.gen.js'
import { call, confirmRisk, connect, output, parseIntegerFlag, type JsonOption } from '../../lib/command-kit.js'
import { ValidationError } from '../../utils/errors.js'

const WORKLOAD_MODES: readonly EnvironmentWorkloadMode[] = ['legacy', 'shadow', 'enforce']

function nodeId(raw: string): number {
  return parseIntegerFlag(raw, '<node-id>', 1)
}

export interface BuildLimitFlags {
  timeout?: string
  concurrency?: string
  queueWait?: string
}

/** Build limits from flags over the current ones; `null` when no flag was given. */
export function buildLimitsRequest(current: BuildLimits, flags: BuildLimitFlags): BuildLimits | null {
  if (flags.timeout === undefined && flags.concurrency === undefined && flags.queueWait === undefined) return null
  return {
    build_timeout_seconds:
      flags.timeout === undefined ? current.build_timeout_seconds : parseIntegerFlag(flags.timeout, '--timeout', 1),
    concurrency: flags.concurrency === undefined ? current.concurrency : parseIntegerFlag(flags.concurrency, '--concurrency', 1),
    queue_wait_seconds:
      flags.queueWait === undefined ? current.queue_wait_seconds : parseIntegerFlag(flags.queueWait, '--queue-wait', 0),
  }
}

export interface IngressFlags {
  enable?: boolean
  disable?: boolean
  httpPort?: string
  httpsPort?: string
  listenAddress?: string
  publicAddress?: string
}

/** The public-ingress settings to save; `null` when no flag was given. Empty addresses mean automatic. */
export function ingressSettingsRequest(
  current: UpdatePublicIngressSettingsRequest,
  flags: IngressFlags,
): UpdatePublicIngressSettingsRequest | null {
  if (flags.enable && flags.disable) throw new ValidationError('Pass --enable or --disable, not both')
  const given = [flags.enable, flags.disable, flags.httpPort, flags.httpsPort, flags.listenAddress, flags.publicAddress]
  if (given.every((v) => v === undefined)) return null
  const port = (raw: string | undefined, flag: string, fallback: number | null | undefined) =>
    raw === undefined ? (fallback ?? null) : parseIntegerFlag(raw, flag, 1, 65535)
  const address = (raw: string | undefined, fallback: string | null | undefined) =>
    raw === undefined ? (fallback ?? null) : raw.trim() || null
  return {
    enabled: flags.enable ? true : flags.disable ? false : current.enabled,
    http_port: port(flags.httpPort, '--http-port', current.http_port),
    https_port: port(flags.httpsPort, '--https-port', current.https_port),
    listen_address: address(flags.listenAddress, current.listen_address),
    public_address: address(flags.publicAddress, current.public_address),
  }
}

export function registerNodesAdminCommands(nodes: Command): void {
  nodes
    .command('show <node-id>')
    .description('Show one node: status, addresses, labels and capacity')
    .option('--json', 'Output in JSON format')
    .action(async (id: string, options: JsonOption) => {
      const node = nodeId(id)
      await connect()
      output(await call(`Could not read node ${node}`, () => adminGetNode({ client, path: { node_id: node } })), options, `Node ${node}`)
    })

  nodes
    .command('containers <node-id>')
    .description('List the containers running on a node')
    .option('--json', 'Output in JSON format')
    .action(async (id: string, options: JsonOption) => {
      const node = nodeId(id)
      await connect()
      const result = await call(`Could not list the containers of node ${node}`, () =>
        adminListNodeContainers({ client, path: { node_id: node } }),
      )
      output(result, options, `Containers on node ${node}`)
    })

  const drain = nodes
    .command('drain <node-id>')
    .description('Stop scheduling on a node and move its replicas elsewhere (redeploys the affected environments)')
    .option('--status', 'Only show drain progress')
    .option('-y, --yes', 'Do not ask for confirmation')
    .option('--json', 'Output in JSON format')
  drain.action(async (id: string, options: JsonOption & { status?: boolean; yes?: boolean }) => {
    const node = nodeId(id)
    await connect()
    if (options.status) {
      output(await call(`Could not read the drain status of node ${node}`, () => adminDrainStatus({ client, path: { node_id: node } })), options, `Drain of node ${node}`)
      return
    }
    if (!(await confirmRisk([`Draining node ${node} redeploys every environment with replicas on it.`], `Drain node ${node}?`, options.yes))) return
    const result = await call(`Could not drain node ${node}`, () => adminDrainNode({ client, path: { node_id: node } }))
    output(result, options, undefined, `Node ${node} is draining. Follow it with: temps nodes drain ${node} --status`)
  })

  nodes
    .command('undrain <node-id>')
    .description('Let a drained node accept new deployments again')
    .option('--json', 'Output in JSON format')
    .action(async (id: string, options: JsonOption) => {
      const node = nodeId(id)
      await connect()
      output(await call(`Could not undrain node ${node}`, () => adminUndrainNode({ client, path: { node_id: node } })), options, undefined, `Node ${node} accepts deployments again`)
    })

  nodes
    .command('remove <node-id>')
    .alias('rm')
    .description('Remove a drained node from the cluster and from every placement that targets it')
    .option('--force', 'Remove even if the node is not drained (its containers are not moved first)')
    .option('-y, --yes', 'Do not ask for confirmation')
    .option('--json', 'Output in JSON format')
    .action(async (id: string, options: JsonOption & { force?: boolean; yes?: boolean }) => {
      const node = nodeId(id)
      await connect()
      const lines = [`Removing node ${node} takes it out of the cluster and every placement that targets it.`]
      if (options.force) lines.push('--force skips the drain check: containers still on it are not rescheduled first.')
      if (!(await confirmRisk(lines, `Remove node ${node}?`, options.yes))) return
      const result = await call(`Could not remove node ${node}`, () =>
        adminRemoveNode({ client, path: { node_id: node }, query: options.force ? { force: true } : undefined }),
      )
      output(result, options, undefined, `Node ${node} removed`)
    })

  nodes
    .command('build-limits <node-id>')
    .description("Show a worker node's build limits, or change them with flags")
    .option('--timeout <seconds>', 'Longest a build may run')
    .option('--concurrency <n>', 'Builds that may run at once')
    .option('--queue-wait <seconds>', 'Longest a build may wait for a slot')
    .option('--json', 'Output in JSON format')
    .action(async (id: string, options: JsonOption & BuildLimitFlags) => {
      const node = nodeId(id)
      await connect()
      const current = await call(`Could not read the build limits of node ${node}`, () =>
        adminGetNodeBuildLimits({ client, path: { node_id: node } }),
      )
      const next = buildLimitsRequest(current, options)
      if (!next) {
        output(current, options, `Build limits of node ${node}`)
        return
      }
      const saved = await call(`Could not save the build limits of node ${node}`, () =>
        adminSetNodeBuildLimits({ client, path: { node_id: node }, body: next }),
      )
      output(saved, options, undefined, `Build limits of node ${node} saved`)
    })

  nodes
    .command('public-ingress <node-id>')
    .description('Show or change whether a node serves public HTTP(S) traffic itself, and on which ports and addresses')
    .option('--enable', 'Serve public traffic on this node')
    .option('--disable', 'Stop serving public traffic on this node')
    .option('--http-port <port>', 'HTTP port to listen on')
    .option('--https-port <port>', 'HTTPS port to listen on')
    .option('--listen-address <addr>', 'Local address to bind ("" for automatic)')
    .option('--public-address <addr>', 'Address DNS points at ("" for the listen address)')
    .option('--toggle-only', 'Only flip enabled with --enable/--disable, without saving ports or addresses')
    .option('--check', 'Check from the control plane that the node answers on its public address')
    .option('-y, --yes', 'Do not ask before disabling')
    .option('--json', 'Output in JSON format')
    .action(async (id: string, options: JsonOption & IngressFlags & { check?: boolean; toggleOnly?: boolean; yes?: boolean }) => {
      const node = nodeId(id)
      // Before any branch: --toggle-only and --check must not pick one of two contradictory switches.
      if (options.enable && options.disable) throw new ValidationError('Pass --enable or --disable, not both')
      await connect()
      if (options.check) {
        output(await call(`Could not check the public ingress of node ${node}`, () => adminCheckNodePublicIngress({ client, path: { node_id: node } })), options, `Public ingress check of node ${node}`)
        return
      }
      if (options.disable && !(await confirmRisk([`Node ${node} stops serving public traffic; DNS pointing at it fails.`], 'Disable public ingress?', options.yes))) return
      if (options.toggleOnly) {
        if (!options.enable && !options.disable) throw new ValidationError('--toggle-only needs --enable or --disable')
        const result = await call(`Could not change the public ingress of node ${node}`, () =>
          adminSetNodePublicIngress({ client, path: { node_id: node }, body: { enabled: Boolean(options.enable) } }),
        )
        output(result, options, undefined, `Public ingress ${options.enable ? 'enabled' : 'disabled'} on node ${node}`)
        return
      }
      const current = await call(`Could not read the public ingress of node ${node}`, () =>
        adminGetNodePublicIngressSettings({ client, path: { node_id: node } }),
      )
      const next = ingressSettingsRequest(current, options)
      if (!next) {
        output(current, options, `Public ingress of node ${node}`)
        return
      }
      const saved = await call(`Could not save the public ingress of node ${node}`, () =>
        adminUpdateNodePublicIngressSettings({ client, path: { node_id: node }, body: next }),
      )
      output(saved, options, undefined, `Public ingress of node ${node} saved`)
    })

  nodes
    .command('workload-mode <environment-id> [mode]')
    .description(`Show an environment's workload mode, or set it (${WORKLOAD_MODES.join(', ')}); operator-only`)
    .option('-y, --yes', 'Do not ask before changing it')
    .option('--json', 'Output in JSON format')
    .action(async (envId: string, mode: string | undefined, options: JsonOption & { yes?: boolean }) => {
      const environment = parseIntegerFlag(envId, '<environment-id>', 1)
      if (mode !== undefined && !WORKLOAD_MODES.includes(mode as EnvironmentWorkloadMode)) {
        throw new ValidationError(`Unknown workload mode "${mode}": use one of ${WORKLOAD_MODES.join(', ')}`)
      }
      await connect()
      if (mode === undefined) {
        output(await call(`Could not read the workload mode of environment ${environment}`, () => adminGetEnvironmentWorkloadMode({ client, path: { environment_id: environment } })), options, `Environment ${environment}`)
        return
      }
      if (!(await confirmRisk([`Changing the workload mode of environment ${environment} changes how its workloads are placed.`], `Set it to ${mode}?`, options.yes))) return
      const result = await call(`Could not set the workload mode of environment ${environment}`, () =>
        adminSetEnvironmentWorkloadMode({ client, path: { environment_id: environment }, body: { workload_mode: mode as EnvironmentWorkloadMode } }),
      )
      output(result, options, undefined, `Environment ${environment} workload mode: ${mode}`)
    })
}
