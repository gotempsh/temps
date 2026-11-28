// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type {
  ProjectIpAccessResponse,
  SetProjectIpAccessRequest,
  UptimeMonitorIpBlock,
} from '@/api/client/types.gen'
import {
  allowMonitor,
  allowMonitorNotice,
  allowMonitorTarget,
  type AllowMonitorTarget,
  type PolicyIo,
} from './allow-monitor'

const warning: UptimeMonitorIpBlock = {
  project_id: 2,
  environment_id: 7,
  policy_scope: 'project',
  mode: 'allow',
  reason: 'not_allow_listed',
  monitor_addresses: ['127.0.0.1'],
  blocked_addresses: ['127.0.0.1'],
  matched_cidrs: [],
  suggested_cidrs: ['127.0.0.1/32'],
  suggested_description: 'Temps uptime monitor',
  message:
    "Blocked by this project's IP access list: Temps' uptime monitor connects from 127.0.0.1, which the list does not allow.",
}

function stored(
  revision: number,
  cidrs: string[],
  block: UptimeMonitorIpBlock | null = warning
): ProjectIpAccessResponse {
  return {
    project_id: 2,
    environment_id: null,
    configured: true,
    mode: 'allow',
    cidrs: cidrs.map((cidr) => ({ cidr })),
    max_cidrs: 256,
    revision,
    warnings: [],
    uptime_monitor_block: block,
  }
}

const conflict = { status: 409, title: 'Policy Changed Since Read' }
const lockout = { status: 409, title: 'Policy Would Lock You Out' }

/**
 * A server holding one list: `reads` are what successive reads return, and
 * `fail` decides whether a write is refused.
 */
function server(
  reads: ProjectIpAccessResponse[],
  fail: (body: SetProjectIpAccessRequest) => unknown = () => null
) {
  const writes: SetProjectIpAccessRequest[] = []
  const targets: AllowMonitorTarget[] = []
  let read = 0
  const io: PolicyIo = {
    read: async (target) => {
      targets.push(target)
      const next = reads[Math.min(read, reads.length - 1)]
      read += 1
      return next
    },
    write: async (_target, body) => {
      writes.push(body)
      const error = fail(body)
      if (error) throw error
      return stored((body.expected_revision ?? 0) + 1, [])
    },
  }
  return { io, writes, targets }
}

describe('allowMonitor', () => {
  test('adds the monitor to the list it read, at the revision it read', async () => {
    const { io, writes } = server([stored(4, ['198.51.100.7/32'])])
    const outcome = await allowMonitor(warning, false, io)
    expect(outcome.kind).toBe('saved')
    expect(writes).toEqual([
      {
        mode: 'allow',
        cidrs: [
          { cidr: '198.51.100.7/32', description: null },
          { cidr: '127.0.0.1/32', description: 'Temps uptime monitor' },
        ],
        acknowledge_lockout: false,
        expected_revision: 4,
      },
    ])
  })

  test('a save that lost a race re-reads and keeps the entry saved meanwhile', async () => {
    const { io, writes } = server(
      [
        stored(4, ['198.51.100.7/32']),
        stored(5, ['198.51.100.7/32', '192.0.2.8/32']),
      ],
      (body) => (body.expected_revision === 4 ? conflict : null)
    )
    const outcome = await allowMonitor(warning, false, io)
    expect(outcome.kind).toBe('saved')
    expect(writes).toHaveLength(2)
    expect(writes[1].expected_revision).toBe(5)
    expect(writes[1].cidrs.map((entry) => entry.cidr)).toEqual([
      '198.51.100.7/32',
      '192.0.2.8/32',
      '127.0.0.1/32',
    ])
  })

  test('gives up on a list that keeps changing', async () => {
    const { io, writes } = server([stored(4, [])], () => conflict)
    await expect(allowMonitor(warning, false, io)).rejects.toEqual(conflict)
    expect(writes).toHaveLength(3)
  })

  test('a lockout refusal is passed on, and retried only when acknowledged', async () => {
    const refusing = server([stored(4, [])], (body) =>
      body.acknowledge_lockout ? null : lockout
    )
    await expect(allowMonitor(warning, false, refusing.io)).rejects.toEqual(
      lockout
    )
    expect(refusing.writes).toHaveLength(1)

    const outcome = await allowMonitor(warning, true, refusing.io)
    expect(outcome.kind).toBe('saved')
    expect(refusing.writes[1].acknowledge_lockout).toBe(true)
  })

  test('writes nothing when the list no longer refuses the monitor', async () => {
    const { io, writes } = server([stored(5, ['127.0.0.1/32'], null)])
    const outcome = await allowMonitor(warning, false, io)
    expect(outcome).toEqual({ kind: 'already-allowed' })
    expect(writes).toHaveLength(0)
  })

  test('writes nothing to a list that became a deny list', async () => {
    const { io, writes } = server([
      {
        ...stored(5, ['127.0.0.0/8']),
        mode: 'deny',
        uptime_monitor_block: {
          ...warning,
          mode: 'deny',
          reason: 'deny_listed',
          matched_cidrs: ['127.0.0.0/8'],
          suggested_cidrs: [],
        },
      },
    ])
    const outcome = await allowMonitor(warning, false, io)
    expect(outcome).toEqual({ kind: 'changed' })
    expect(writes).toHaveLength(0)
  })

  test('writes nothing to a full list', async () => {
    const full = Array.from({ length: 256 }, (_, i) => `10.0.${i}.0/24`)
    const { io, writes } = server([stored(5, full)])
    const outcome = await allowMonitor(warning, false, io)
    expect(outcome).toEqual({ kind: 'full', needed: 257, max: 256 })
    expect(writes).toHaveLength(0)
  })

  test("reads and writes the environment's own list when that refuses it", async () => {
    const own = { ...warning, policy_scope: 'environment' as const }
    const { io, targets } = server([
      { ...stored(4, []), environment_id: 7, uptime_monitor_block: own },
    ])
    await allowMonitor(own, false, io)
    expect(targets).toEqual([{ projectId: 2, environmentId: 7 }])
  })
})

describe('allowMonitorTarget', () => {
  test('a project block is fixed on the project list', () => {
    expect(allowMonitorTarget(warning)).toEqual({
      projectId: 2,
      environmentId: null,
    })
  })
})

describe('allowMonitorNotice', () => {
  test('says how many entries to remove from a full list', () => {
    expect(allowMonitorNotice({ kind: 'full', needed: 258, max: 256 })).toBe(
      'The list holds at most 256 entries. Remove 2 to make room for the monitor.'
    )
  })
})
