// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { describeDockerSocketAccess } from './show.js'
import type { DockerSocketCapability } from '../../api/types.gen.js'

function makeCapability(overrides: Partial<DockerSocketCapability> = {}): DockerSocketCapability {
  return {
    granted: false,
    nodes: [],
    reason: "No host grants project 'node-daemon' access to the Docker socket.",
    setup_path: '/settings/nodes',
    ...overrides,
  }
}

describe('describeDockerSocketAccess', () => {
  test('names every host that grants the project', () => {
    expect(
      describeDockerSocketAccess(
        makeCapability({ granted: true, nodes: ['control-plane', 'worker-1'], reason: null })
      )
    ).toBe('granted on control-plane, worker-1')
  })

  test('says granted even when the host list is empty', () => {
    expect(
      describeDockerSocketAccess(makeCapability({ granted: true, reason: null }))
    ).toBe('granted')
  })

  test('repeats the server reason so the operator knows what to set', () => {
    const line = describeDockerSocketAccess(makeCapability())
    expect(line).toStartWith('not granted — ')
    expect(line).toContain('node-daemon')
  })

  test('falls back to its own sentence when an older server sends no reason', () => {
    expect(describeDockerSocketAccess(makeCapability({ reason: null }))).toBe(
      'not granted — no host grants this project access to the Docker socket'
    )
  })

  test('an absent capability prints no row at all', () => {
    // A response that never computed the capability must not be reported as a
    // denial — the field is attached to detail responses only.
    expect(describeDockerSocketAccess(null)).toBeNull()
    expect(describeDockerSocketAccess(undefined)).toBeNull()
  })
})
