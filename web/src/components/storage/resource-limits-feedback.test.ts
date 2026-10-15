// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  RECREATE_REQUIRED_DESCRIPTION,
  resourceUpdateMessages,
} from './resource-limits-feedback'

describe('resourceUpdateMessages', () => {
  test('a refused /dev/shm change does not hide a memory cap that still needs a restart', () => {
    const messages = resourceUpdateMessages({
      shm_not_applied:
        'Refusing to change service 3: its data would not be carried over',
      applied: [
        { container_name: 'postgres-a-1', outcome: 'requires_recreate' },
        {
          container_name: 'postgres-a-2',
          outcome: 'failed',
          error: 'below current usage',
        },
      ],
    })
    expect(messages.map((m) => m.title)).toEqual([
      'Limits saved, but the shared memory size was not applied',
      'Limits saved, but some containers rejected the update',
      'Limits saved — restart required',
    ])
    expect(messages[0].description).toContain('would not be carried over')
    expect(messages[1].description).toBe('postgres-a-2: below current usage')
    expect(messages[2].description).toBe(RECREATE_REQUIRED_DESCRIPTION)
    expect(messages.every((m) => m.kind === 'warning')).toBe(true)
  })

  test('a refused /dev/shm change alone is one warning, never a success', () => {
    const messages = resourceUpdateMessages({
      shm_not_applied: 'refused',
      applied: [{ container_name: 'redis-c', outcome: 'applied' }],
    })
    expect(messages).toHaveLength(1)
    expect(messages[0].kind).toBe('warning')
  })

  test('everything applied live reports success with counts', () => {
    expect(
      resourceUpdateMessages({
        applied: [
          { container_name: 'a', outcome: 'applied' },
          { container_name: 'b', outcome: 'stopped' },
        ],
      })
    ).toEqual([
      {
        kind: 'success',
        title: 'Resource limits applied',
        description: '1 live · 1 on next start',
      },
    ])
  })

  test('no containers yet: saved for next start', () => {
    expect(resourceUpdateMessages({ applied: [] })[0].title).toBe(
      'Resource limits saved'
    )
  })
})
