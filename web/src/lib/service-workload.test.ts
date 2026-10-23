// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { jsonResponse, mockApi, restoreAllMockApis } from '@/test/fetch-mock'
import {
  getServiceWorkloadPendingChanges,
  recreateAvailability,
  recreateServiceFromWorkloadSpec,
  serviceLocation,
  serviceModeChangeConsequence,
  serviceWorkloadModeSummary,
  setServiceWorkloadMode,
  type ServiceWorkloadModeStatus,
  type ServiceWorkloadPendingChanges,
} from './service-workload'

afterEach(() => restoreAllMockApis())

function status(
  overrides: Partial<ServiceWorkloadModeStatus> = {}
): ServiceWorkloadModeStatus {
  return {
    service_id: 12,
    service_name: 'orders-db',
    service_type: 'postgres',
    mode: 'enforce',
    support: { supported: true },
    ...overrides,
  }
}

function pending(
  overrides: Partial<ServiceWorkloadPendingChanges> = {}
): ServiceWorkloadPendingChanges {
  return {
    service_id: 12,
    service_name: 'orders-db',
    mode: 'enforce',
    container_name: 'postgres-orders-db',
    support: { supported: true },
    container_found: true,
    changes: [{ field: 'pids_limit', current: 'none', after_recreate: '4096' }],
    fingerprint: 'abc123',
    computed_at: '2026-01-01T00:00:00Z',
    ...overrides,
  }
}

describe('requests', () => {
  test('sets the mode with the server field name', async () => {
    let body: unknown
    mockApi({
      'PUT /external-services/12/workload-mode': async (request) => {
        body = await request.json()
        return jsonResponse({
          previous_mode: 'legacy',
          workload: status(),
          pending_changes: pending(),
        })
      },
    })
    const change = await setServiceWorkloadMode(12, 'enforce')
    expect(body).toEqual({ mode: 'enforce' })
    expect(change.previous_mode).toBe('legacy')
    expect(change.pending_changes?.fingerprint).toBe('abc123')
  })

  test('reads the pending changes', async () => {
    mockApi({
      'GET /external-services/12/workload-mode/pending-changes': () =>
        jsonResponse(pending()),
    })
    const changes = await getServiceWorkloadPendingChanges(12)
    expect(changes.changes[0].field).toBe('pids_limit')
  })

  test('a recreate always sends the confirmed fingerprint', async () => {
    let body: unknown
    mockApi({
      'POST /external-services/12/workload-mode/recreate': async (request) => {
        body = await request.json()
        return jsonResponse({
          status: 'recreated',
          pending_changes: pending(),
          remaining_changes: pending({ changes: [] }),
        })
      },
    })
    const result = await recreateServiceFromWorkloadSpec(12, 'abc123')
    expect(body).toEqual({ confirmed_fingerprint: 'abc123' })
    expect(result.status).toBe('recreated')
    expect(result.remaining_changes?.changes).toEqual([])
  })

  test('a stale confirmation rejects with the server detail', async () => {
    mockApi({
      'POST /external-services/12/workload-mode/recreate': () =>
        jsonResponse(
          {
            title: 'Pending Changes Changed',
            detail: 'The pending changes changed since they were shown',
          },
          409
        ),
    })
    await expect(
      recreateServiceFromWorkloadSpec(12, 'old')
    ).rejects.toMatchObject({ status: 409, title: 'Pending Changes Changed' })
  })
})

describe('recreateAvailability', () => {
  test('allowed in enforce with changes on a found container', () => {
    expect(recreateAvailability(status(), pending())).toEqual({
      allowed: true,
      reason: null,
    })
  })

  test('explains an unsupported database with the server reason', () => {
    expect(
      recreateAvailability(
        status({
          mode: 'legacy',
          support: { supported: false, reason: 'imported container' },
        }),
        pending()
      )
    ).toEqual({ allowed: false, reason: 'imported container' })
  })

  test('requires enforce mode', () => {
    expect(
      recreateAvailability(status({ mode: 'shadow' }), pending()).reason
    ).toContain('only available in Enforce mode')
  })

  test('refuses without a container or without changes', () => {
    expect(
      recreateAvailability(
        status({ node_id: 4 }),
        pending({ container_found: false, node_id: 4 })
      ).reason
    ).toContain('on node 4')
    expect(
      recreateAvailability(status(), pending({ changes: [] })).reason
    ).toContain('already matches')
    expect(recreateAvailability(status(), undefined).allowed).toBe(false)
  })
})

describe('summaries', () => {
  test('locates the database', () => {
    expect(serviceLocation(undefined)).toBe('the control plane')
    expect(serviceLocation(4)).toBe('node 4')
  })

  test('enforce says nothing running changes until a recreate', () => {
    expect(serviceWorkloadModeSummary(status())).toContain(
      'unchanged until then'
    )
    expect(serviceModeChangeConsequence('enforce')).toContain(
      'Nothing running changes now'
    )
    expect(serviceModeChangeConsequence('legacy')).toContain(
      'Nothing running changes'
    )
  })
})
