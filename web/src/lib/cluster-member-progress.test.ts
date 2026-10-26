// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { memberProgress } from './cluster-member-progress'

describe('memberProgress', () => {
  test('says where a creating member is starting', () => {
    expect(
      memberProgress({
        role: 'monitor',
        status: 'creating',
        node_id: 2,
        provisioning_step: 'provisioning_container',
      })
    ).toEqual({
      tone: 'progress',
      text: 'Pulling the image and starting the container on node 2.',
    })
  })

  test('explains the wait for the monitor to come up', () => {
    expect(
      memberProgress({
        role: 'monitor',
        status: 'creating',
        node_id: null,
        provisioning_step: 'waiting_for_health',
      })?.text
    ).toContain('on the control plane; waiting for it to accept connections')
  })

  test('tells queued data nodes what they wait for', () => {
    expect(memberProgress({ role: 'replica', status: 'pending' })?.text).toBe(
      'Queued: created after the monitor is ready.'
    )
  })

  test('shows the recorded error on the member that failed', () => {
    expect(
      memberProgress({
        role: 'monitor',
        status: 'failed',
        provisioning_step: 'failed',
        provisioning_error: 'Container x on node 2 did not become ready',
      })
    ).toEqual({
      tone: 'error',
      text: 'Container x on node 2 did not become ready',
    })
  })

  test('a failed member without a recorded reason still says so', () => {
    expect(memberProgress({ role: 'replica', status: 'failed' })?.tone).toBe(
      'error'
    )
  })

  test('shows nothing once the member runs', () => {
    expect(
      memberProgress({
        role: 'replica',
        status: 'running',
        provisioning_step: 'done',
      })
    ).toBeNull()
  })
})
