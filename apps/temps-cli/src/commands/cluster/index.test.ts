// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import {
  classifyNodeDnsHealth,
  formatSyncAge,
  STALE_SYNC_THRESHOLD_SECONDS,
  type NodeDnsStatusEntry,
} from './index.js'

function makeEntry(overrides: Partial<NodeDnsStatusEntry> = {}): NodeDnsStatusEntry {
  return {
    node_id: 1,
    node_name: 'worker-1',
    node_status: 'active',
    dns_resolver_running: true,
    dns_resolver_tasks_alive: true,
    dns_resolver_last_sync_at: new Date().toISOString(),
    seconds_since_last_sync: 5,
    dns_resolver_consecutive_failures: 0,
    dns_resolver_last_error: null,
    dns_resolver_record_count: 12,
    ...overrides,
  }
}

describe('classifyNodeDnsHealth', () => {
  test('never-reported node (null running) is unknown, not disabled', () => {
    // `null` must stay distinct from `false` — a node that has never sent a
    // heartbeat looks nothing like one that reported "resolver is off".
    const entry = makeEntry({
      dns_resolver_running: null,
      dns_resolver_tasks_alive: null,
      dns_resolver_last_sync_at: null,
      seconds_since_last_sync: null,
    })
    expect(classifyNodeDnsHealth(entry)).toBe('unknown')
  })

  test('reported and confirmed off is disabled', () => {
    const entry = makeEntry({ dns_resolver_running: false, dns_resolver_tasks_alive: false })
    expect(classifyNodeDnsHealth(entry)).toBe('disabled')
  })

  test('running but a background task died is unhealthy', () => {
    const entry = makeEntry({ dns_resolver_tasks_alive: false })
    expect(classifyNodeDnsHealth(entry)).toBe('unhealthy')
  })

  test('running with consecutive sync failures is degraded', () => {
    const entry = makeEntry({ dns_resolver_consecutive_failures: 3 })
    expect(classifyNodeDnsHealth(entry)).toBe('degraded')
  })

  test('running, zero failures, but a stale sync is degraded', () => {
    const entry = makeEntry({ seconds_since_last_sync: STALE_SYNC_THRESHOLD_SECONDS + 1 })
    expect(classifyNodeDnsHealth(entry)).toBe('degraded')
  })

  test('running, zero failures, sync within threshold is healthy', () => {
    const entry = makeEntry({ seconds_since_last_sync: STALE_SYNC_THRESHOLD_SECONDS - 1 })
    expect(classifyNodeDnsHealth(entry)).toBe('healthy')
  })

  test('running with no sync data at all (never synced yet) is healthy, not stale', () => {
    // `seconds_since_last_sync: null` means "never synced", which is
    // expected right after the resolver starts — not the same as "synced a
    // long time ago", so it must not be flagged degraded on that basis alone.
    const entry = makeEntry({ dns_resolver_last_sync_at: null, seconds_since_last_sync: null })
    expect(classifyNodeDnsHealth(entry)).toBe('healthy')
  })
})

describe('formatSyncAge', () => {
  test('never-synced node reports "never"', () => {
    expect(formatSyncAge(makeEntry({ dns_resolver_last_sync_at: null }))).toBe('never')
  })

  test('recently synced node reports a relative time', () => {
    const entry = makeEntry({ dns_resolver_last_sync_at: new Date().toISOString() })
    expect(formatSyncAge(entry)).toBe('just now')
  })
})
