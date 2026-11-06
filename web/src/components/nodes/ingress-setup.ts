// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// "Expose your apps": the Cloudflare Tunnel setup as one run over the
// existing endpoints (managed domain, tunnel, worker status), with live
// progress and an undo of exactly what the run created.

import {
  addManagedDomain,
  adminListNodes,
  createIngressConnector,
  deleteDnsProvider,
  deleteIngressConnector,
  getIngressConnector,
  listIngressConnectors,
  removeManagedDomain,
  updateManagedDomain,
  verifyManagedDomain,
} from '@/api/client/sdk.gen'
import type {
  ConnectorView,
  CreateConnectorRequest,
  IngressMode,
  ManagedDomainResponse,
  NodeInfoResponse,
} from '@/api/client/types.gen'
import {
  deliveryError,
  requireDeliveryData,
} from '@/components/domains/delivery-errors'
import { tunnelNameFor } from '@/components/domains/ZoneTunnelChoice'
import { replicaState } from '@/components/nodes/WorkerTunnels'
import { hostSuffixError } from '@/lib/tunnel-host-suffix'
import { ALLOW_INGRESS_COMMANDS, workerIngressWay } from '@/lib/worker-ingress'
import { WORKER_NODES_URL } from '@/lib/worker-nodes'

/** The page; under Worker Nodes, where the question comes up. */
export const INGRESS_SETUP_URL = `${WORKER_NODES_URL}/ingress`

export type IngressWay = 'tunnel' | 'ports'

/** A link to the page, optionally for one worker and one way in. */
export function ingressSetupUrl(
  options: { worker?: number; via?: IngressWay } = {}
): string {
  const params = new URLSearchParams()
  if (options.via) params.set('via', options.via)
  if (options.worker != null) params.set('worker', String(options.worker))
  const query = params.toString()
  return query ? `${INGRESS_SETUP_URL}?${query}` : INGRESS_SETUP_URL
}

/** `temps_ingress::cloudflare::KIND`. */
export const CLOUDFLARE_TUNNEL_KIND = 'cloudflare_tunnel'

/**
 * Apps directly under the zone (`shop.example.com`), one level under it with
 * a label appended (`shop-lab.example.com`, the tunnel's `host_suffix`), or
 * under a subdomain (`shop.lab.example.com`, two levels down).
 */
export type HostnameScheme =
  | { kind: 'zone' }
  | { kind: 'suffix'; label: string }
  | { kind: 'subdomain'; label: string }

const DNS_LABELS =
  /^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)*$/

export function normalizeLabel(label: string): string {
  return label.trim().toLowerCase().replace(/\.+$/, '')
}

/** Why a subdomain label is unusable, or `undefined` when it is fine. */
export function subdomainLabelError(label: string): string | undefined {
  const value = normalizeLabel(label)
  if (!value) return 'Enter the subdomain, for example lab.'
  if (!DNS_LABELS.test(value)) {
    return 'Use letters, digits, hyphens and dots only.'
  }
  return undefined
}

/** Why the scheme's label is unusable, or `undefined` when it is fine. */
export function schemeLabelError(scheme: HostnameScheme): string | undefined {
  if (scheme.kind === 'suffix') return hostSuffixError(scheme.label)
  if (scheme.kind === 'subdomain') return subdomainLabelError(scheme.label)
  return undefined
}

/**
 * The domain the tunnel serves: the zone (also with a suffix, whose hosts
 * sit directly under it), or `label.zone` for a subdomain.
 */
export function tunnelDomainFor(zone: string, scheme: HostnameScheme): string {
  const base = zone.trim().toLowerCase().replace(/\.$/, '')
  if (scheme.kind !== 'subdomain') return base
  return `${normalizeLabel(scheme.label)}.${base}`
}

/** The tunnel's `host_suffix` for the scheme, if it has one. */
export function hostSuffixFor(scheme: HostnameScheme): string | undefined {
  return scheme.kind === 'suffix' ? normalizeLabel(scheme.label) : undefined
}

/** An example app hostname under the scheme: `shop-lab.example.com`. */
export function exampleHost(
  app: string,
  zone: string,
  scheme: HostnameScheme
): string {
  const base = zone.trim().toLowerCase().replace(/\.$/, '')
  const label = scheme.kind === 'zone' ? '' : normalizeLabel(scheme.label)
  if (scheme.kind === 'suffix') return `${app}-${label || 'lab'}.${base}`
  if (scheme.kind === 'subdomain') return `${app}.${label || 'lab'}.${base}`
  return `${app}.${base}`
}

/** A tunnel name from the domain that no existing tunnel uses. */
export function uniqueTunnelName(domain: string, taken: string[]): string {
  const base = tunnelNameFor(domain) || 'tunnel'
  const names = new Set(taken.map((n) => n.toLowerCase()))
  if (!names.has(base)) return base
  for (let n = 2; ; n++) {
    const candidate = `${base}-${n}`
    if (!names.has(candidate)) return candidate
  }
}

/** A DNS provider name no existing provider uses. */
export function uniqueProviderName(base: string, taken: string[]): string {
  const names = new Set(taken.map((n) => n.trim().toLowerCase()))
  if (!names.has(base.toLowerCase())) return base
  for (let n = 2; ; n++) {
    const candidate = `${base} ${n}`
    if (!names.has(candidate.toLowerCase())) return candidate
  }
}

/** Whether a worker can take a tunnel now, and whether to pick it by default. */
export interface WorkerEligibility {
  selectable: boolean
  /** Already reachable through a tunnel or open ports. */
  hasIngress: boolean
  /** Picked unless the operator changes it. */
  defaultOn: boolean
  /** Why it cannot be picked. */
  reason?: string
  /** What to run on the worker to make it pickable. */
  command?: string
}

export function workerEligibility(
  node: Pick<
    NodeInfoResponse,
    | 'id'
    | 'role'
    | 'status'
    | 'public_ingress_enabled'
    | 'public_ingress_running'
  >,
  mode: IngressMode | null | undefined,
  tunnels: { provider: string; nodes: { node_id: number }[] }[]
): WorkerEligibility {
  const hasIngress = workerIngressWay(node, tunnels).kind !== 'none'
  if (node.role !== 'worker') {
    return {
      selectable: false,
      hasIngress,
      defaultOn: false,
      reason: 'Only workers run tunnels.',
    }
  }
  if (node.status !== 'active') {
    return {
      selectable: false,
      hasIngress,
      defaultOn: false,
      reason: offlineWorkerReason(node.status),
      // Only a stopped agent is fixed by starting it; a drained or disabled
      // worker is brought back from its page.
      command: node.status === 'offline' ? START_AGENT_COMMAND : undefined,
    }
  }
  // Agents that report no mode predate the rule and are accepted (ADR 058 D7).
  if (mode === 'off' || mode === 'fixed') {
    return {
      selectable: false,
      hasIngress,
      defaultOn: false,
      reason:
        mode === 'off'
          ? "It doesn't take public traffic, and only the worker can change that."
          : 'It manages its own ingress, so Temps cannot add a tunnel to it.',
      command: ALLOW_INGRESS_COMMANDS.delegated,
    }
  }
  return { selectable: true, hasIngress, defaultOn: !hasIngress }
}

/** Starts a worker's agent where `temps agent service install` set it up. */
export const START_AGENT_COMMAND = 'sudo systemctl start temps-agent'

/** Why a worker that is not active cannot run a tunnel, and how to fix it. */
export function offlineWorkerReason(status: string): string {
  return status === 'offline'
    ? 'It is offline, so it cannot run a tunnel. Start its agent on that machine: with the systemd service run the command below, otherwise start `temps agent` under its supervisor again. It shows as active here within a minute.'
    : `It is ${status}, so it cannot run a tunnel. Bring it back from its page under Worker nodes.`
}

// ----- the run --------------------------------------------------------------

export type StepStatus = 'pending' | 'running' | 'done' | 'failed'

export interface SetupStep {
  key: string
  label: string
  status: StepStatus
  detail?: string
}

/** What the run is asked to do. The DNS provider exists before it starts. */
export interface SetupPlan {
  provider: { id: number; name: string; createdThisRun: boolean }
  zone: string
  /** The zone's managed-domain row under this provider, if it has one. */
  managed: ManagedDomainResponse | undefined
  tunnelDomain: string
  /** Environment hosts become `<label>-<hostSuffix>.<tunnelDomain>`. */
  hostSuffix?: string
  tunnelName: string
  nodes: { id: number; name: string }[]
}

/** Everything this run created or changed, so undo touches nothing else. */
export interface SetupCreated {
  providerId?: number
  managedDomain?: { providerId: number; domain: string }
  /** An existing managed domain whose automatic DNS this run turned on. */
  autoManageEnabled?: { providerId: number; domain: string }
  connectorId?: number
}

export interface UndoResult {
  what: 'connector' | 'auto_manage' | 'managed_domain' | 'provider'
  label: string
  ok: boolean
  error?: string
}

export type SetupOutcome =
  | { kind: 'done'; connector: ConnectorView; created: SetupCreated }
  /** Created, but a worker has not connected in time: kept, not undone. */
  | { kind: 'waiting'; connector: ConnectorView; created: SetupCreated }
  | {
      kind: 'failed'
      error: string
      created: SetupCreated
      undo: UndoResult[]
      /**
       * The run stopped before it changed anything (the workers check), so
       * nothing was undone: an account added on this page stays for a retry.
       */
      stoppedBeforeChanges?: boolean
    }
  | { kind: 'aborted'; created: SetupCreated }

/** The endpoints the run uses; the console's SDK in the app, fakes in tests. */
export interface IngressSetupApi {
  addManagedDomain(
    providerId: number,
    domain: string
  ): Promise<ManagedDomainResponse>
  enableAutoManage(
    providerId: number,
    domain: string,
    enabled: boolean
  ): Promise<void>
  verifyManagedDomain(
    providerId: number,
    domain: string
  ): Promise<ManagedDomainResponse>
  removeManagedDomain(providerId: number, domain: string): Promise<void>
  createConnector(request: CreateConnectorRequest): Promise<ConnectorView>
  /** The nodes as they are now, to re-check the workers before creating. */
  listNodes(): Promise<
    Pick<NodeInfoResponse, 'id' | 'name' | 'role' | 'status'>[]
  >
  findConnectorByName(name: string): Promise<ConnectorView | undefined>
  getConnector(id: number): Promise<ConnectorView>
  deleteConnector(id: number): Promise<void>
  deleteDnsProvider(id: number): Promise<void>
  sleep(ms: number, signal?: AbortSignal): Promise<void>
}

async function noContent(response: Promise<{ error?: unknown }>) {
  const { error } = await response
  if (error) throw new Error(deliveryError(error))
}

export const sdkIngressSetupApi: IngressSetupApi = {
  addManagedDomain: async (providerId, domain) =>
    requireDeliveryData(
      await addManagedDomain({
        path: { id: providerId },
        body: { domain, auto_manage: true },
      })
    ),
  enableAutoManage: async (providerId, domain, enabled) => {
    requireDeliveryData(
      await updateManagedDomain({
        path: { provider_id: providerId, domain },
        body: { auto_manage: enabled },
      })
    )
  },
  verifyManagedDomain: async (providerId, domain) =>
    requireDeliveryData(
      await verifyManagedDomain({
        path: { provider_id: providerId, domain },
      })
    ),
  removeManagedDomain: (providerId, domain) =>
    noContent(
      removeManagedDomain({ path: { provider_id: providerId, domain } })
    ),
  createConnector: async (request) =>
    requireDeliveryData(await createIngressConnector({ body: request })),
  findConnectorByName: async (name) => {
    // Newest first, so a connector this run just saved is on the first page.
    const list = requireDeliveryData(
      await listIngressConnectors({ query: { page: 1, page_size: 100 } })
    )
    return list.connectors.find((c) => c.name === name)
  },
  getConnector: async (id) =>
    requireDeliveryData(await getIngressConnector({ path: { id } })),
  listNodes: async () => requireDeliveryData(await adminListNodes()).nodes,
  deleteConnector: (id) => noContent(deleteIngressConnector({ path: { id } })),
  deleteDnsProvider: (id) => noContent(deleteDnsProvider({ path: { id } })),
  sleep: (ms, signal) =>
    new Promise((resolve) => {
      const timer = setTimeout(resolve, ms)
      signal?.addEventListener(
        'abort',
        () => {
          clearTimeout(timer)
          resolve()
        },
        { once: true }
      )
    }),
}

/** How long to wait for workers to connect before handing over. */
export const CONNECT_TIMEOUT_MS = 180_000
export const POLL_INTERVAL_MS = 2_000

function message(error: unknown): string {
  return error instanceof Error ? error.message : deliveryError(error)
}

/** Remove what a run created, newest first. Never touches anything else. */
export async function undoSetup(
  created: SetupCreated,
  api: IngressSetupApi
): Promise<UndoResult[]> {
  const results: UndoResult[] = []
  const attempt = async (
    what: UndoResult['what'],
    label: string,
    action: () => Promise<void>
  ) => {
    try {
      await action()
      results.push({ what, label, ok: true })
    } catch (error) {
      results.push({ what, label, ok: false, error: message(error) })
    }
  }
  if (created.connectorId != null) {
    const id = created.connectorId
    await attempt('connector', 'Deleted the tunnel and its DNS records', () =>
      api.deleteConnector(id)
    )
  }
  if (created.autoManageEnabled) {
    const { providerId, domain } = created.autoManageEnabled
    await attempt(
      'auto_manage',
      `Turned automatic DNS for ${domain} back off`,
      () => api.enableAutoManage(providerId, domain, false)
    )
  }
  if (created.managedDomain) {
    const { providerId, domain } = created.managedDomain
    await attempt('managed_domain', `Removed ${domain} from Temps`, () =>
      api.removeManagedDomain(providerId, domain)
    )
  }
  if (created.providerId != null) {
    const id = created.providerId
    await attempt(
      'provider',
      'Removed the Cloudflare account added on this page',
      () => api.deleteDnsProvider(id)
    )
  }
  return results
}

class StepFailure extends Error {}

/**
 * The first of `chosen` that cannot run a tunnel now, and why. The page's
 * node list can be minutes old, so the run reads it again before it creates
 * anything: a worker that went offline would otherwise fail only at the
 * tunnel, after the domain was added.
 */
export function unavailableWorker(
  chosen: { id: number; name: string }[],
  nodes: Pick<NodeInfoResponse, 'id' | 'name' | 'role' | 'status'>[]
): string | undefined {
  for (const pick of chosen) {
    const node = nodes.find((n) => n.id === pick.id)
    if (!node) {
      return `Worker '${pick.name}' is no longer in this cluster. Untick it under Workers and try again.`
    }
    if (node.role !== 'worker') {
      return `Node '${node.name}' is a ${node.role}, not a worker; only workers run tunnels.`
    }
    if (node.status !== 'active') {
      const fix =
        node.status === 'offline'
          ? `Start its agent on that machine (${START_AGENT_COMMAND} with the systemd service, otherwise \`temps agent\` under its supervisor)`
          : 'Bring it back from its page under Worker nodes'
      return `Worker '${node.name}' is ${node.status}, so it cannot run a tunnel. ${fix} or untick it under Workers, then try again.`
    }
  }
  return undefined
}

/**
 * Domain, tunnel, then each worker's connection, reporting every change.
 * A failure undoes what the run created; a worker that is merely slow is
 * not a failure, and the run hands over with everything kept.
 */
export async function runTunnelSetup(
  plan: SetupPlan,
  api: IngressSetupApi,
  report: (steps: SetupStep[]) => void,
  signal?: AbortSignal,
  now: () => number = Date.now
): Promise<SetupOutcome> {
  const created: SetupCreated = plan.provider.createdThisRun
    ? { providerId: plan.provider.id }
    : {}
  const steps: SetupStep[] = [
    {
      key: 'account',
      label: `Cloudflare account ${plan.provider.name}`,
      status: 'done',
      detail: plan.provider.createdThisRun
        ? 'Added with the token you pasted; Cloudflare accepted it.'
        : 'Already in Temps.',
    },
    {
      key: 'workers',
      label: `Workers ${plan.nodes.map((n) => n.name).join(', ')} online`,
      status: 'pending',
    },
    { key: 'domain', label: `Domain ${plan.zone}`, status: 'pending' },
    {
      key: 'tunnel',
      label: `Tunnel ${plan.tunnelName} for ${plan.tunnelDomain}`,
      status: 'pending',
    },
    ...plan.nodes.map((node) => ({
      key: `node-${node.id}`,
      label: `Connected on ${node.name}`,
      status: 'pending' as StepStatus,
    })),
  ]
  const set = (key: string, status: StepStatus, detail?: string) => {
    const step = steps.find((s) => s.key === key)
    if (step) {
      step.status = status
      step.detail = detail
    }
    report(steps.map((s) => ({ ...s })))
  }
  const fail = async (key: string, error: string): Promise<SetupOutcome> => {
    set(key, 'failed', error)
    const undo = await undoSetup(created, api)
    return { kind: 'failed', error, created, undo }
  }
  // Nothing is undone here: the run has changed nothing yet, and an account
  // added with a pasted token stays so the user can fix the worker and retry.
  const stopBeforeChanges = (error: string): SetupOutcome => {
    set('workers', 'failed', error)
    return {
      kind: 'failed',
      error,
      created,
      undo: [],
      stoppedBeforeChanges: true,
    }
  }
  report(steps.map((s) => ({ ...s })))

  // 0. The workers, read again: nothing is created for a tunnel that
  // cannot run.
  set('workers', 'running', 'Checking they are connected…')
  try {
    const unavailable = unavailableWorker(plan.nodes, await api.listNodes())
    if (unavailable) return stopBeforeChanges(unavailable)
  } catch (error) {
    return stopBeforeChanges(message(error))
  }
  set('workers', 'done', 'Connected.')

  // A page that went away (or a new run) stops this one before it creates
  // anything, so nothing is made without a page to show it and undo it.
  // Past this point the run finishes what it started rather than stopping
  // halfway with a domain and no tunnel.
  if (signal?.aborted) return { kind: 'aborted', created }

  // 1. The domain, managed and verified: a tunnel only takes one.
  set('domain', 'running', 'Adding it to Temps…')
  try {
    let managed = plan.managed
    if (!managed) {
      managed = await api.addManagedDomain(plan.provider.id, plan.zone)
      created.managedDomain = {
        providerId: plan.provider.id,
        domain: plan.zone,
      }
    } else if (!managed.auto_manage) {
      await api.enableAutoManage(plan.provider.id, plan.zone, true)
      created.autoManageEnabled = {
        providerId: plan.provider.id,
        domain: plan.zone,
      }
    }
    if (!managed.verified) {
      set('domain', 'running', 'Checking the token can manage it…')
      const verified = await api.verifyManagedDomain(
        plan.provider.id,
        plan.zone
      )
      if (!verified.verified) {
        throw new StepFailure(
          verified.zone_access_error ??
            verified.verification_error ??
            `Cloudflare did not confirm the token can manage ${plan.zone}. Add the zone to the token's Zone Resources, then try again.`
        )
      }
    }
    set(
      'domain',
      'done',
      created.managedDomain ? 'Added and verified.' : 'Already in Temps.'
    )
  } catch (error) {
    return fail('domain', message(error))
  }

  // 2. The tunnel. A save that fails afterwards still leaves a row, found
  // by its name: the name was free when the run started.
  set('tunnel', 'running', 'Creating it in Cloudflare…')
  let connector: ConnectorView
  try {
    connector = await api.createConnector({
      name: plan.tunnelName,
      provider: CLOUDFLARE_TUNNEL_KIND,
      values: { dns_provider_id: plan.provider.id, domain: plan.tunnelDomain },
      // Sent only with one, so a tunnel without a suffix asks exactly what
      // it asked before suffixes existed.
      ...(plan.hostSuffix ? { host_suffix: plan.hostSuffix } : {}),
      node_ids: plan.nodes.map((n) => n.id),
    })
    created.connectorId = connector.id
  } catch (error) {
    try {
      const saved = await api.findConnectorByName(plan.tunnelName)
      if (saved) created.connectorId = saved.id
    } catch {
      // Without the list there is no row we know of to clean up.
    }
    return fail('tunnel', message(error))
  }
  if (connector.status === 'error') {
    return fail(
      'tunnel',
      connector.last_error ?? 'Cloudflare refused the tunnel.'
    )
  }
  set('tunnel', 'done', 'Created, with its DNS records managed by Temps.')

  // 3. Each worker picks it up and connects.
  const deadline = now() + CONNECT_TIMEOUT_MS
  for (const node of plan.nodes) set(`node-${node.id}`, 'running', 'Waiting…')
  for (;;) {
    if (signal?.aborted) return { kind: 'aborted', created }
    try {
      connector = await api.getConnector(connector.id)
    } catch (error) {
      // A missed poll is not a failure; the deadline still applies.
      for (const node of plan.nodes) {
        const step = steps.find((s) => s.key === `node-${node.id}`)
        if (step?.status === 'running') {
          set(
            `node-${node.id}`,
            'running',
            `Status unavailable: ${message(error)}`
          )
        }
      }
    }
    if (connector.status === 'error') {
      return fail(
        'tunnel',
        connector.last_error ?? 'Cloudflare refused the tunnel.'
      )
    }
    let settled = 0
    let connected = 0
    for (const node of plan.nodes) {
      const replica = connector.nodes.find((n) => n.node_id === node.id)
      const key = `node-${node.id}`
      if (!replica) {
        set(key, 'running', 'Waiting for the worker to pick it up')
        continue
      }
      const state = replicaState(replica)
      if (replica.state === 'ready') {
        settled++
        connected++
        const links = replica.ready_connections
        set(
          key,
          'done',
          links
            ? `${links} connection${links === 1 ? '' : 's'} to Cloudflare`
            : undefined
        )
      } else if (replica.state === 'refused') {
        settled++
        set(
          key,
          'failed',
          replica.last_error
            ? `${state.label}: ${replica.last_error}`
            : state.label
        )
      } else {
        set(
          key,
          'running',
          replica.last_error
            ? `${state.label}: ${replica.last_error}`
            : state.label
        )
      }
    }
    if (settled === plan.nodes.length) {
      if (connected > 0) return { kind: 'done', connector, created }
      return failAll(connector)
    }
    if (now() >= deadline) return { kind: 'waiting', connector, created }
    await api.sleep(POLL_INTERVAL_MS, signal)
  }

  async function failAll(view: ConnectorView): Promise<SetupOutcome> {
    const reasons = view.nodes
      .map((n) => n.last_error)
      .filter((r): r is string => !!r)
    const error = `No worker could run the tunnel${reasons.length ? `: ${reasons.join('; ')}` : '.'}`
    const undo = await undoSetup(created, api)
    return { kind: 'failed', error, created, undo }
  }
}
