// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { InstantiationResponse, InstantiationStatus } from '@/api/client'
import {
  followInstantiation,
  InstantiationPollError,
} from './follow-instantiation'

function instantiation(status: InstantiationStatus): InstantiationResponse {
  return {
    id: 7,
    kind: 'template',
    name: 'Shop',
    status,
    services: [],
    steps: [],
    created_at: '2026-09-29T00:00:00Z',
    updated_at: '2026-09-29T00:00:00Z',
  }
}

const noSleep = async () => {}

describe('followInstantiation', () => {
  test('polls until the instantiation finishes', async () => {
    const statuses: InstantiationStatus[] = ['pending', 'running', 'succeeded']
    const seen: InstantiationStatus[] = []
    const outcome = await followInstantiation(7, {
      signal: new AbortController().signal,
      fetchInstantiation: async () => instantiation(statuses.shift()!),
      onUpdate: (i) => seen.push(i.status),
      sleep: noSleep,
    })
    expect(outcome.kind).toBe('finished')
    expect(seen).toEqual(['pending', 'running', 'succeeded'])
  })

  test('stops at the deadline while it is still running', async () => {
    let clock = 0
    const outcome = await followInstantiation(7, {
      signal: new AbortController().signal,
      fetchInstantiation: async () => instantiation('running'),
      now: () => clock,
      sleep: async (ms) => {
        clock += ms
      },
      intervalMs: 1000,
      timeoutMs: 3000,
    })
    expect(outcome.kind).toBe('timed_out')
    expect(outcome.instantiation?.status).toBe('running')
  })

  test('stops when aborted', async () => {
    const controller = new AbortController()
    const outcome = await followInstantiation(7, {
      signal: controller.signal,
      fetchInstantiation: async () => {
        controller.abort()
        return instantiation('running')
      },
      sleep: noSleep,
    })
    expect(outcome.kind).toBe('aborted')
  })

  test('tolerates a failed poll, then names the instantiation', async () => {
    let calls = 0
    const recovered = await followInstantiation(7, {
      signal: new AbortController().signal,
      fetchInstantiation: async () => {
        calls += 1
        if (calls === 1) throw new Error('502')
        return instantiation('failed')
      },
      sleep: noSleep,
    })
    expect(recovered.kind).toBe('finished')

    const lost = followInstantiation(7, {
      signal: new AbortController().signal,
      fetchInstantiation: async () => {
        throw new Error('502')
      },
      sleep: noSleep,
    })
    await expect(lost).rejects.toBeInstanceOf(InstantiationPollError)
    await expect(lost).rejects.toThrow('#7')
  })
})
