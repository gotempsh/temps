// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * A node's detail page is split into sections, each a page of its own with
 * an entry in the sidebar, like a project's. The overview answers "is this
 * node healthy and what runs on it"; the rest are what an operator changes
 * now and then. This module decides which sections a node has and which
 * problems the overview leads with.
 */

import {
  Activity,
  Hammer,
  LayoutDashboard,
  Network,
  SlidersHorizontal,
  type LucideIcon,
} from 'lucide-react'
import type {
  AlarmResponse,
  NodeDnsStatusEntry,
  NodeInfoResponse,
} from '@/api/client'
import { nodeIsUnreachable, WORKER_NODES_URL } from '@/lib/worker-nodes'

export type NodeSection =
  'overview' | 'monitoring' | 'networking' | 'builds' | 'scheduling'

export interface NodeSectionLink {
  section: NodeSection
  title: string
  icon: LucideIcon
}

const OVERVIEW: NodeSectionLink = {
  section: 'overview',
  title: 'Overview',
  icon: LayoutDashboard,
}
const MONITORING: NodeSectionLink = {
  section: 'monitoring',
  title: 'Monitoring',
  icon: Activity,
}
const NETWORKING: NodeSectionLink = {
  section: 'networking',
  title: 'Networking',
  icon: Network,
}
const BUILDS: NodeSectionLink = {
  section: 'builds',
  title: 'Builds',
  icon: Hammer,
}
const SCHEDULING: NodeSectionLink = {
  section: 'scheduling',
  title: 'Scheduling',
  icon: SlidersHorizontal,
}

/**
 * The sections a node of `role` has. Only workers take public traffic and
 * run builds; edge nodes run no workloads, so they have no network policy.
 */
export function nodeSections(role: string | undefined): NodeSectionLink[] {
  switch (role) {
    case 'worker':
      return [OVERVIEW, MONITORING, NETWORKING, BUILDS, SCHEDULING]
    case 'control-plane':
      return [OVERVIEW, MONITORING, NETWORKING, SCHEDULING]
    default:
      return [OVERVIEW, MONITORING, SCHEDULING]
  }
}

/**
 * The section a URL segment names, for a node of `role`. A missing segment,
 * an unknown one, or one this node does not have (an old link to a worker's
 * builds after it became an edge node) is the overview.
 */
export function resolveNodeSection(
  segment: string | undefined,
  role: string | undefined
): NodeSection {
  const found = nodeSections(role).find((link) => link.section === segment)
  return found?.section ?? 'overview'
}

export function nodeSectionUrl(nodeId: number, section: NodeSection): string {
  const base = `${WORKER_NODES_URL}/${nodeId}`
  return section === 'overview' ? base : `${base}/${section}`
}

/** The node id and section in a pathname, when it is a node's page. */
export function nodeRouteOf(
  pathname: string
): { nodeId: number; segment: string | undefined } | null {
  const match = pathname.match(/^\/settings\/nodes\/(\d+)(?:\/([^/]+))?\/?$/)
  if (!match) return null
  return { nodeId: Number(match[1]), segment: match[2] }
}

function silentFor(heartbeat: string, now: number): string {
  const minutes = Math.max(
    1,
    Math.round((now - Date.parse(heartbeat)) / 60_000)
  )
  if (minutes < 60) return `${minutes} minute${minutes === 1 ? '' : 's'}`
  const hours = Math.round(minutes / 60)
  if (hours < 48) return `${hours} hour${hours === 1 ? '' : 's'}`
  return `${Math.round(hours / 24)} days`
}

export interface NodeAlert {
  key: string
  tone: 'error' | 'warn'
  title: string
  description: string
  action?: { label: string; to: string }
}

export interface NodeAlertInput {
  node: {
    id: number
    name: string
    status: string
    last_heartbeat?: string | null
    compute_cidr?: string | null
    overlay_status?: string | null
    overlay_last_error?: string | null
    public_ingress_enabled: boolean
    public_ingress_last_error?: string | null
  }
  /** This node's resolver as the cluster DNS status reports it. */
  dns?: {
    enabled: boolean
    running?: boolean | null
    lastError?: string | null
    /** Applications on this node created without its resolver. */
    applicationsWithoutResolver: number
  }
  /** Alarms of this node's alert rules that are firing now. */
  firingAlarms?: {
    id: number
    title: string
    severity: string
    message?: string | null
  }[]
  now?: number
}

/**
 * What needs attention on a node, most severe first, each with where to fix
 * it. Empty for a healthy node: the overview shows nothing rather than a
 * row of green checks. Draining and removal blockers are not listed here;
 * the overview shows those with their own progress.
 */
export function nodeOverviewAlerts(input: NodeAlertInput): NodeAlert[] {
  const { node, dns, firingAlarms = [] } = input
  const networking = nodeSectionUrl(node.id, 'networking')
  const monitoring = nodeSectionUrl(node.id, 'monitoring')
  const alerts: NodeAlert[] = []

  const draining = node.status === 'draining' || node.status === 'drained'
  if (
    !draining &&
    (node.status === 'offline' || nodeIsUnreachable(node, input.now))
  ) {
    alerts.push({
      key: 'unreachable',
      tone: 'error',
      title: `${node.name} is not reporting`,
      description: node.last_heartbeat
        ? `No heartbeat for ${silentFor(node.last_heartbeat, input.now ?? Date.now())}. Its workloads may still run, but nothing new is placed on it and its status here is out of date. Check that the agent is running on the machine (systemctl status temps-agent).`
        : 'Its agent has never reported. Check that it is running on the machine (systemctl status temps-agent).',
    })
  }

  for (const alarm of firingAlarms) {
    alerts.push({
      key: `alarm-${alarm.id}`,
      tone: alarm.severity === 'critical' ? 'error' : 'warn',
      title: alarm.title,
      description: alarm.message ?? 'An alert rule of this node is firing.',
      action: { label: 'Open monitoring', to: monitoring },
    })
  }

  if (dns?.enabled && dns.running === false) {
    alerts.push({
      key: 'dns-down',
      tone: 'error',
      title: 'Cluster DNS resolver is down on this node',
      description: `Applications here cannot resolve *.temps.local names until it is back.${dns.lastError ? ` Last error: ${dns.lastError}` : ''}`,
      action: { label: 'Open networking', to: networking },
    })
  }

  if (node.compute_cidr && node.overlay_status === 'unavailable') {
    alerts.push({
      key: 'overlay',
      tone: 'warn',
      title: 'Cross-node network unavailable',
      description:
        node.overlay_last_error ??
        'The agent reported the cross-node network unavailable without a cause; check its logs.',
      action: { label: 'Open networking', to: networking },
    })
  }

  if (node.public_ingress_enabled && node.public_ingress_last_error) {
    alerts.push({
      key: 'ingress',
      tone: 'warn',
      title: 'Public traffic is not being served correctly',
      description: node.public_ingress_last_error,
      action: { label: 'Open networking', to: networking },
    })
  }

  if (dns?.enabled && dns.applicationsWithoutResolver > 0) {
    const count = dns.applicationsWithoutResolver
    alerts.push({
      key: 'dns-stale',
      tone: 'warn',
      title: `${count} application${count === 1 ? '' : 's'} here cannot resolve service names`,
      description:
        'They were started before Cluster DNS was on, and a container keeps the nameservers it was created with. Redeploy them.',
      action: {
        label: 'Show applications',
        to: `${WORKER_NODES_URL}#cluster-dns`,
      },
    })
  }

  return alerts.sort(
    (a, b) => Number(b.tone === 'error') - Number(a.tone === 'error')
  )
}

/** The firing alarms that belong to one of `ruleIds`. */
export function alarmsOfRules(
  alarms: AlarmResponse[],
  ruleIds: Set<number>
): AlarmResponse[] {
  return alarms.filter((alarm) => {
    const ruleId = (alarm.metadata as { rule_id?: unknown } | null)?.rule_id
    return typeof ruleId === 'number' && ruleIds.has(ruleId)
  })
}

export type NodeStatusTone = 'ok' | 'warn' | 'muted'

export interface NodeStatusRow {
  label: string
  value: string
  tone: NodeStatusTone
  detail?: string | null
}

/** How this node's cross-node network reads. */
export function overlayRow(
  node: Pick<
    NodeInfoResponse,
    'compute_cidr' | 'overlay_status' | 'overlay_last_error'
  >
): NodeStatusRow {
  if (!node.compute_cidr) {
    return {
      label: 'Cross-node network',
      value: 'No subnet',
      tone: 'muted',
      detail:
        'It gets a subnet of the cluster pool when it joins a multi-node cluster.',
    }
  }
  if (node.overlay_status === 'ready') {
    return {
      label: 'Cross-node network',
      value: `Connected on ${node.compute_cidr}`,
      tone: 'ok',
    }
  }
  if (node.overlay_status === 'unavailable') {
    return {
      label: 'Cross-node network',
      value: 'Unavailable',
      tone: 'warn',
      detail: node.overlay_last_error,
    }
  }
  return { label: 'Cross-node network', value: 'Not reported', tone: 'muted' }
}

/** How this node's cluster DNS resolver reads. */
export function resolverRow(
  enabled: boolean | undefined,
  entry: NodeDnsStatusEntry | undefined
): NodeStatusRow {
  if (enabled === undefined) {
    return { label: 'Cluster DNS resolver', value: 'Unknown', tone: 'muted' }
  }
  if (!enabled) {
    return {
      label: 'Cluster DNS resolver',
      value: 'Cluster DNS is off',
      tone: 'muted',
    }
  }
  if (entry?.dns_resolver_running) {
    return {
      label: 'Cluster DNS resolver',
      value: `Running, ${entry.dns_resolver_record_count ?? 0} names`,
      tone: 'ok',
    }
  }
  if (entry?.dns_resolver_running === false) {
    return {
      label: 'Cluster DNS resolver',
      value: 'Down',
      tone: 'warn',
      detail: entry.dns_resolver_last_error,
    }
  }
  return {
    label: 'Cluster DNS resolver',
    value: 'Not reported',
    tone: 'muted',
  }
}
