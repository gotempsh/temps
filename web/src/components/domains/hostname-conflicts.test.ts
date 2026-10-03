// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { DnsRecordChange, DnsRecordConflict } from '@/api/client'
import {
  conflictDecisionsRequest,
  conflictKey,
  describeRecordValue,
  plannedDnsChanges,
  unresolvedConflicts,
} from './hostname-conflicts'

function conflict(
  name: string,
  overrides: Partial<DnsRecordConflict> = {}
): DnsRecordConflict {
  return {
    name,
    record_type: 'A',
    value: '203.0.113.10',
    proxied: true,
    reason: 'an existing A record at this name has no Temps ownership marker',
    adoptable: true,
    current_value: '198.51.100.7',
    current_proxied: false,
    revision: `revision-${name}`,
    ...overrides,
  }
}

function change(action: string, name: string): DnsRecordChange {
  return { action, name, record_type: 'A', value: '203.0.113.10' }
}

describe('conflictKey', () => {
  test('identifies a conflict by record type and case-insensitive name', () => {
    expect(conflictKey(conflict('PR-1.Example.com'))).toBe('A pr-1.example.com')
    expect(
      conflictKey(conflict('pr-1.example.com', { record_type: 'CNAME' }))
    ).toBe('CNAME pr-1.example.com')
  })
})

describe('unresolvedConflicts', () => {
  const adoptable = conflict('pr-1.example.com')
  const owned = conflict('pr-2.example.com', {
    adoptable: false,
    reason: 'the record is managed by custom domain delivery',
  })
  const ambiguous = conflict('pr-3.example.com', {
    adoptable: false,
    current_value: null,
    current_proxied: null,
  })

  test('every conflict needs a decision', () => {
    expect(unresolvedConflicts([adoptable, owned, ambiguous], {})).toEqual([
      adoptable,
      owned,
      ambiguous,
    ])
  })

  test('skip resolves any conflict, adopt only an adoptable one', () => {
    expect(
      unresolvedConflicts([adoptable, owned, ambiguous], {
        [conflictKey(adoptable)]: 'adopt',
        [conflictKey(owned)]: 'adopt',
        [conflictKey(ambiguous)]: 'skip',
      })
    ).toEqual([owned])
  })

  test('adopting needs the record that would be adopted', () => {
    const withoutRecord = conflict('pr-4.example.com', {
      current_value: null,
    })
    expect(
      unresolvedConflicts([withoutRecord], {
        [conflictKey(withoutRecord)]: 'adopt',
      })
    ).toEqual([withoutRecord])
  })
})

describe('conflictDecisionsRequest', () => {
  test('sends each decided conflict once, with the revision the user reviewed', () => {
    const adopted = conflict('pr-1.example.com')
    const skipped = conflict('pr-2.example.com', { adoptable: false })
    const undecided = conflict('pr-3.example.com')

    expect(
      conflictDecisionsRequest([adopted, skipped, undecided], {
        [conflictKey(adopted)]: 'adopt',
        [conflictKey(skipped)]: 'skip',
      })
    ).toEqual({
      adopt_records: [
        {
          name: 'pr-1.example.com',
          record_type: 'A',
          revision: 'revision-pr-1.example.com',
        },
      ],
      skip_records: [
        {
          name: 'pr-2.example.com',
          record_type: 'A',
          revision: 'revision-pr-2.example.com',
        },
      ],
    })
  })

  test('never sends an adoption the server marked impossible', () => {
    const owned = conflict('pr-1.example.com', { adoptable: false })
    expect(
      conflictDecisionsRequest([owned], { [conflictKey(owned)]: 'adopt' })
    ).toEqual({ adopt_records: [], skip_records: [] })
  })

  test('ignores decisions for conflicts the preview no longer reports', () => {
    expect(
      conflictDecisionsRequest([], { 'A pr-9.example.com': 'skip' })
    ).toEqual({ adopt_records: [], skip_records: [] })
  })
})

describe('plannedDnsChanges', () => {
  test('lists every planned write but not the conflicts', () => {
    const changes = [
      change('conflict', 'pr-1.example.com'),
      change('adopt', 'pr-2.example.com'),
      change('update', 'pr-2.example.com'),
      change('skip', 'pr-3.example.com'),
      change('create', 'pr-4.example.com'),
    ]
    expect(plannedDnsChanges(changes).map((c) => c.action)).toEqual([
      'adopt',
      'update',
      'skip',
      'create',
    ])
  })
})

describe('describeRecordValue', () => {
  test('says how Cloudflare serves the record', () => {
    expect(describeRecordValue('203.0.113.10', true)).toBe(
      '203.0.113.10 (proxied)'
    )
    expect(describeRecordValue('203.0.113.10', false)).toBe(
      '203.0.113.10 (DNS only)'
    )
    expect(describeRecordValue('edge.example.net', null)).toBe(
      'edge.example.net (DNS only)'
    )
  })
})
