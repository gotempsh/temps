// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The cluster compute pool and each node's cross-node overlay (ADR 056):
 * form rules and display helpers over the generated
 * `GET/PUT /settings/cluster-network` bindings.
 */
import { z } from 'zod'

import type { ClusterNetworkNode } from '@/api/client/types.gen'

export type {
  ClusterNetworkChangeResponse,
  ClusterNetworkControlPlane,
  ClusterNetworkNode,
  ClusterNetworkReassignment,
  ClusterNetworkStatus,
  UpdateClusterNetworkRequest,
} from '@/api/client/types.gen'

const IPV4_CIDR = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})\/(\d{1,2})$/

/** A form field and what is wrong with it. */
export interface PoolInputIssue {
  field: 'poolCidr' | 'nodePrefix'
  message: string
}

/**
 * Check the form before sending it, with the message the operator needs and
 * the field it is about. The server repeats every check and adds the ones
 * that need the cluster (mesh pool, node addresses, occupied ranges,
 * allocations in use).
 */
export function poolInputIssue(
  pool: string,
  prefix: string
): PoolInputIssue | null {
  const onPool = (message: string): PoolInputIssue => ({
    field: 'poolCidr',
    message,
  })
  const onPrefix = (message: string): PoolInputIssue => ({
    field: 'nodePrefix',
    message,
  })
  const match = IPV4_CIDR.exec(pool.trim())
  if (!match) {
    return onPool('Enter an IPv4 network such as 10.202.0.0/16.')
  }
  const octets = match.slice(1, 5).map(Number)
  const poolLen = Number(match[5])
  if (octets.some((octet) => octet > 255) || poolLen > 32) {
    return onPool('Enter an IPv4 network such as 10.202.0.0/16.')
  }
  const [a, b] = octets
  const private1918 =
    (a === 10 && poolLen >= 8) ||
    (a === 172 && b >= 16 && b <= 31 && poolLen >= 12) ||
    (a === 192 && b === 168 && poolLen >= 16)
  if (!private1918) {
    return onPool(
      'The pool must be private (RFC 1918) address space: inside 10.0.0.0/8, 172.16.0.0/12 or 192.168.0.0/16.'
    )
  }
  const address =
    ((octets[0] << 24) | (octets[1] << 16) | (octets[2] << 8) | octets[3]) >>> 0
  const mask = poolLen === 0 ? 0 : (0xffffffff << (32 - poolLen)) >>> 0
  if ((address & mask) >>> 0 !== address) {
    return onPool(
      'Host bits are set: use the network address of the range (for example 10.202.0.0/16).'
    )
  }
  const nodePrefix = Number(prefix)
  if (!/^\d{1,2}$/.test(prefix.trim()) || nodePrefix > 30) {
    return onPrefix(
      'The per-node prefix must be a number up to 30, such as 24.'
    )
  }
  if (nodePrefix <= poolLen) {
    return onPrefix(
      `The per-node prefix must be longer than the pool's /${poolLen}, such as /24.`
    )
  }
  return null
}

/** {@link poolInputIssue}'s message alone. */
export function validatePoolInput(pool: string, prefix: string): string | null {
  return poolInputIssue(pool, prefix)?.message ?? null
}

/** The Cluster network form's fields, as typed. */
export const clusterNetworkFormSchema = z
  .object({ poolCidr: z.string(), nodePrefix: z.string() })
  .superRefine((values, ctx) => {
    const issue = poolInputIssue(values.poolCidr, values.nodePrefix)
    if (issue) {
      ctx.addIssue({
        code: 'custom',
        path: [issue.field],
        message: issue.message,
      })
    }
  })

export type ClusterNetworkFormValues = z.infer<typeof clusterNetworkFormSchema>

export interface OverlayStatusView {
  label: string
  tone: 'ok' | 'warn' | 'muted'
  description: string
}

/**
 * How a node's cross-node overlay reads in the node list, with the last
 * automatic move of its subnet when there was one.
 */
export function nodeOverlayStatus(node: ClusterNetworkNode): OverlayStatusView {
  const view = overlayStatusOf(node)
  return node.overlay_remediation
    ? {
        ...view,
        description: `${view.description} Automatically ${node.overlay_remediation}.`,
      }
    : view
}

function overlayStatusOf(node: ClusterNetworkNode): OverlayStatusView {
  if (!node.compute_cidr) {
    return {
      label: 'No subnet',
      tone: 'muted',
      description:
        'This node has no subnet of the cluster pool yet; it gets one when it joins a multi-node cluster.',
    }
  }
  if (node.overlay_status === 'ready') {
    return {
      label: 'Connected',
      tone: 'ok',
      description: `Cross-node network up on ${node.compute_cidr}.`,
    }
  }
  if (node.overlay_status === 'unavailable') {
    return {
      label: 'Cross-node network unavailable',
      tone: 'warn',
      description:
        node.overlay_last_error ??
        'The agent reported the cross-node network unavailable without a cause; check its logs.',
    }
  }
  return {
    label: 'Not reported',
    tone: 'muted',
    description:
      'Its agent has not reported the cross-node network yet. Upgrade the agent if this persists.',
  }
}
