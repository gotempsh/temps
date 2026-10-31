// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { containerActionErrorMessage } from './ContainerActionDialog'

describe('containerActionErrorMessage', () => {
  test('says why the API refused, from the Problem it answered with', () => {
    const problem = {
      title: 'Invalid Deployment State',
      status: 400,
      detail:
        'Cannot start container vm-1: it runs in a Firecracker microVM (workload 7), which cannot be stopped, started or restarted on its own',
    }
    expect(containerActionErrorMessage('start', problem)).toBe(
      `Failed to start container: ${problem.detail}`
    )
  })

  test('falls back to an error message, then to a generic one', () => {
    expect(containerActionErrorMessage('stop', new Error('network down'))).toBe(
      'Failed to stop container: network down'
    )
    expect(containerActionErrorMessage('restart', undefined)).toBe(
      'Failed to restart container: Unknown error'
    )
  })
})
