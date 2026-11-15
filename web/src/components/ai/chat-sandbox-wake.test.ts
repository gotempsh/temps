// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ChatSandboxStatus } from '@/api/client'
import { describe, expect, test } from 'bun:test'
import { needsWake } from './chat-sandbox-wake'

const base: ChatSandboxStatus = {
  configured: true,
  offered: true,
  connected_projects: [],
  connected_services: [],
}
const linked = {
  sandbox_id: 'sbx_1',
  name: 'chat-sandbox',
  linked_at: '2026-10-18T12:00:00Z',
}

describe('needsWake', () => {
  test('only a linked sandbox that reports asleep is started', () => {
    expect(needsWake({ ...base, linked, runtime: 'asleep' })).toBe(true)
    expect(needsWake({ ...base, linked, runtime: 'running' })).toBe(false)
  })

  test('nothing is started when nothing is linked or the state is unknown', () => {
    expect(needsWake(undefined)).toBe(false)
    expect(needsWake({ ...base, runtime: 'asleep' })).toBe(false)
    expect(needsWake({ ...base, linked })).toBe(false)
  })
})
