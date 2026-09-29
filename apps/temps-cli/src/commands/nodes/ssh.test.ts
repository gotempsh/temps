// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { describeEnrollment, newLogLines } from './ssh.js'
import type { NodeSshEnrollmentResponse } from '../../api/types.gen.js'

function makeEnrollment(
  overrides: Partial<NodeSshEnrollmentResponse> = {}
): NodeSshEnrollmentResponse {
  return {
    id: 1,
    name: 'worker-1',
    host: 'node.example.com',
    ssh_address: '198.51.100.7:22',
    ssh_user: 'root',
    auth_method: 'password',
    host_key_fingerprint: `SHA256:${'A'.repeat(43)}`,
    pairing_id: 3,
    status: 'running',
    step: 'installing temps',
    log: '',
    error: null,
    agent_mode: null,
    node_id: null,
    created_at: '2026-09-29T10:00:00Z',
    finished_at: null,
    ...overrides,
  }
}

describe('describeEnrollment', () => {
  test('names the step it is on or stopped at', () => {
    expect(describeEnrollment(makeEnrollment())).toBe('running: installing temps')
    expect(describeEnrollment(makeEnrollment({ status: 'failed', step: 'pairing' }))).toBe(
      'failed while pairing'
    )
  })

  test('warns when the agent will not survive a reboot', () => {
    expect(
      describeEnrollment(makeEnrollment({ status: 'succeeded', agent_mode: 'service' }))
    ).toBe('added')
    expect(
      describeEnrollment(makeEnrollment({ status: 'succeeded', agent_mode: 'detached' }))
    ).toContain('stops at reboot')
  })
})

describe('newLogLines', () => {
  test('returns only complete lines not printed yet', () => {
    const first = newLogLines('a\nb\npart', 0)
    expect(first.lines).toEqual(['a', 'b'])
    const second = newLogLines('a\nb\npartial\nc\n', first.printed)
    expect(second.lines).toEqual(['partial', 'c'])
    expect(newLogLines('a\nb\npartial\nc\n', second.printed).lines).toEqual([])
  })

  test('starts over when the server trimmed the log below what was printed', () => {
    expect(newLogLines('x\n', 50).lines).toEqual(['x'])
  })
})
