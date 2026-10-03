// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import type { DnsRecordConflict } from '@/api/client'
import { HostnameConflictList } from './HostnameConflictList'
import { conflictKey, type ConflictDecisions } from './hostname-conflicts'

const unmarked: DnsRecordConflict = {
  name: 'pr-1.example.com',
  record_type: 'A',
  value: '203.0.113.10',
  proxied: true,
  reason:
    'an existing A record at this name has no Temps ownership marker, and Temps never overwrites a record it does not manage',
  adoptable: true,
  current_value: '198.51.100.7',
  current_proxied: false,
  revision: 'revision-1',
}

const delivery: DnsRecordConflict = {
  name: 'pr-2.example.com',
  record_type: 'A',
  value: '203.0.113.10',
  proxied: true,
  reason:
    'the record is managed by custom domain delivery, which this sync never takes over',
  adoptable: false,
  current_value: '198.51.100.8',
  current_proxied: true,
  revision: 'revision-2',
}

function render(
  conflicts: DnsRecordConflict[],
  decisions: ConflictDecisions = {}
): string {
  return renderToStaticMarkup(
    <HostnameConflictList
      conflicts={conflicts}
      decisions={decisions}
      onDecide={() => {}}
    />
  )
}

describe('HostnameConflictList', () => {
  test('offers to adopt an unmarked record, showing its current and new value', () => {
    const html = render([unmarked])

    expect(html).toContain('1 DNS record needs a decision')
    expect(html).toContain('A pr-1.example.com')
    expect(html).toContain('no Temps ownership marker')
    expect(html).toContain('198.51.100.7 (DNS only) → 203.0.113.10 (proxied)')
    expect(html).toContain('Adopt this record and point it at')
    expect(html).toContain('Skip this hostname and leave its record untouched')
    expect(html).not.toContain('can’t be adopted')
  })

  test('only offers to skip a record another workflow owns, and says why', () => {
    const html = render([unmarked, delivery])

    expect(html).toContain('2 DNS records need a decision')
    expect(html).toContain('managed by custom domain delivery')
    expect(html).toContain('This record can’t be adopted.')
    // One adopt option, for the unmarked record only; a skip option for each.
    expect(html.match(/role="radio"[^>]*value="adopt"/g)).toHaveLength(1)
    expect(html.match(/role="radio"[^>]*value="skip"/g)).toHaveLength(2)
  })

  test('shows the decision the user made', () => {
    const undecided = render([unmarked])
    expect(undecided).not.toContain('aria-checked="true"')

    const skipped = render([unmarked], { [conflictKey(unmarked)]: 'skip' })
    expect(skipped.match(/aria-checked="true"/g)).toHaveLength(1)
    expect(skipped).toMatch(/role="radio" aria-checked="true"[^>]*value="skip"/)
  })
})
