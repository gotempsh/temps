// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import type { ServiceTlsStatusResponse } from '@/api/client/types.gen'
import {
  clusterTlsLine,
  confirmCopy,
  containerTlsLine,
  memberTlsLabel,
  TLS_SETTLE_POLL_MS,
  TLS_SETTLE_WINDOW_MS,
  tlsModeOptions,
  tlsRefetchInterval,
  tlsVerdict,
} from './service-tls'

const offStatus: ServiceTlsStatusResponse = {
  service_id: 7,
  mode: 'off',
  supported: true,
  unsupported_reason: null,
  accepts_plaintext: true,
  active: false,
  pending_restart: false,
  change_restarts_database: true,
  members: [],
  certificate: null,
  certificate_authority: null,
  ca_certificate_path: null,
}

const availableStatus: ServiceTlsStatusResponse = {
  ...offStatus,
  mode: 'available',
  active: true,
  certificate: {
    subject_alt_names: ['postgres-orders', 'localhost', '127.0.0.1'],
    not_before: '2026-11-10T00:00:00Z',
    not_after: '2027-11-10T00:00:00Z',
    fingerprint_sha256: 'aabb',
  },
  certificate_authority: {
    fingerprint_sha256: 'abcdef01',
    not_before: '2026-11-10T00:00:00Z',
    not_after: '2036-11-08T00:00:00Z',
  },
  ca_certificate_path: '/external-services/7/tls/ca-certificate',
}

const managedNode = {
  container_name: 'postgres-orders-1',
  status: 'running',
  active: true,
  self_signed: false,
  pending: false,
}

const clusterStatus: ServiceTlsStatusResponse = {
  ...availableStatus,
  change_restarts_database: false,
  members: [
    managedNode,
    { ...managedNode, container_name: 'postgres-orders-2' },
  ],
}

describe('tlsVerdict', () => {
  it('names each state', () => {
    expect(tlsVerdict(offStatus)).toEqual({ tone: 'idle', label: 'Off' })
    expect(tlsVerdict(availableStatus)).toEqual({
      tone: 'ok',
      label: 'Available',
    })
    expect(
      tlsVerdict({ ...availableStatus, pending_restart: true, active: false })
    ).toEqual({ tone: 'warn', label: 'Restart pending' })
    expect(tlsVerdict({ ...availableStatus, active: false }).label).toBe(
      'Not active'
    )
    expect(tlsVerdict({ ...offStatus, supported: false }).label).toBe(
      'Not supported'
    )
  })
})

describe('unknown TLS', () => {
  it('is never shown as healthy', () => {
    expect(tlsVerdict({ ...availableStatus, active: null })).toEqual({
      tone: 'idle',
      label: 'Unknown',
    })
    // A stopped cluster: its data nodes could not be checked.
    const stoppedCluster = {
      ...clusterStatus,
      active: null,
      members: clusterStatus.members.map((m) => ({
        ...m,
        status: 'stopped',
        active: null,
      })),
    }
    expect(tlsVerdict(stoppedCluster).label).toBe('Unknown')
    expect(tlsVerdict({ ...clusterStatus, members: [] }).label).toBe('Unknown')
    // One node unknown is enough.
    expect(
      tlsVerdict({
        ...clusterStatus,
        members: [clusterStatus.members[0], { ...managedNode, active: null }],
      }).label
    ).toBe('Unknown')
    // A cluster whose nodes all serve it is healthy, though its own
    // `active` is never set.
    expect(tlsVerdict({ ...clusterStatus, active: null })).toEqual({
      tone: 'ok',
      label: 'Available',
    })
  })
})

describe('tlsRefetchInterval', () => {
  const settling = {
    ...clusterStatus,
    members: [clusterStatus.members[0], { ...managedNode, pending: true }],
  }
  it('re-reads a running database whose TLS is still settling', () => {
    expect(tlsRefetchInterval(settling, 'running', 0)).toBe(TLS_SETTLE_POLL_MS)
    expect(
      tlsRefetchInterval({ ...availableStatus, active: null }, 'starting', 0)
    ).toBe(TLS_SETTLE_POLL_MS)
    expect(
      tlsRefetchInterval(
        { ...availableStatus, pending_restart: true },
        'running',
        1_000
      )
    ).toBe(TLS_SETTLE_POLL_MS)
  })
  it('stops once settled, stopped, or long after the change', () => {
    expect(tlsRefetchInterval(availableStatus, 'running', 0)).toBe(false)
    expect(tlsRefetchInterval(settling, 'stopped', 0)).toBe(false)
    expect(
      tlsRefetchInterval(settling, 'running', TLS_SETTLE_WINDOW_MS + 1)
    ).toBe(false)
    expect(tlsRefetchInterval(undefined, 'running', 0)).toBe(false)
    expect(
      tlsRefetchInterval({ ...settling, supported: false }, 'running', 0)
    ).toBe(false)
  })
})

describe('containerTlsLine', () => {
  it('says when the container is missing, pending or serving', () => {
    expect(containerTlsLine({ ...offStatus, active: null })).toContain(
      'unknown'
    )
    expect(
      containerTlsLine({
        ...availableStatus,
        active: false,
        pending_restart: true,
      })
    ).toContain('next start')
    expect(containerTlsLine(availableStatus)).toContain('offers TLS')
  })
})

describe('cluster TLS wording', () => {
  it('names a data node that has not switched yet', () => {
    const status: ServiceTlsStatusResponse = {
      ...clusterStatus,
      pending_restart: true,
      active: null,
      members: [
        managedNode,
        {
          ...managedNode,
          container_name: 'postgres-orders-2',
          active: false,
          self_signed: true,
          pending: true,
        },
      ],
    }
    expect(tlsVerdict(status)).toEqual({
      tone: 'warn',
      label: 'Not on every node',
    })
    expect(clusterTlsLine(status)).toBe(
      'Managed certificate on 1 of 2 data nodes. The rest get it at the next start of the cluster.'
    )
    expect(memberTlsLabel(status.members[1])).toBe(
      'Self-signed certificate (pending)'
    )
  })

  it('reports a node that could not be checked', () => {
    const line = memberTlsLabel({
      ...managedNode,
      active: null,
      status: 'stopped',
      detail: 'the member is stopped',
    })
    expect(line).toBe('Unknown: the member is stopped')
    expect(
      containerTlsLine({
        ...clusterStatus,
        active: null,
        members: [{ ...managedNode, active: null }, managedNode],
      })
    ).toContain('1 could not be checked.')
  })

  it('describes Off as self-signed TLS, not plaintext only', () => {
    const off = tlsModeOptions(clusterStatus).find((o) => o.value === 'off')
    expect(off?.description).toContain('self-signed')
    expect(
      tlsModeOptions(offStatus).find((o) => o.value === 'off')?.description
    ).toBe('The database accepts only plaintext connections.')
    expect(
      clusterTlsLine({
        ...clusterStatus,
        mode: 'off',
        members: [{ ...managedNode, active: false, self_signed: true }],
      })
    ).toBe('Self-signed certificate on 1 of 1 data node.')
  })

  it('says a cluster change restarts nothing and a standalone one does', () => {
    expect(confirmCopy('orders', 'available', false).description).toContain(
      'nothing restarts'
    )
    expect(confirmCopy('orders', 'off', false).confirmLabel).toBe(
      'Use self-signed'
    )
    expect(confirmCopy('orders', 'available', true).description).toContain(
      'restarted with brief downtime'
    )
  })
})
