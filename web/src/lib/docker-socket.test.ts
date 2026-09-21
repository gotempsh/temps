// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { DockerSocketCapability } from '@/api/client/types.gen'
import {
  describeDockerSocket,
  shouldShowDockerSocketBadge,
  shouldShowDockerSocketOnboarding,
} from './docker-socket'

function capability(
  overrides: Partial<DockerSocketCapability> = {}
): DockerSocketCapability {
  return {
    granted: false,
    nodes: [],
    reason: "No host grants project 'node-daemon' access to the Docker socket.",
    setup_path: '/settings/nodes',
    ...overrides,
  }
}

describe('describeDockerSocket', () => {
  test('a granted project names the hosts that grant it', () => {
    const description = describeDockerSocket(
      capability({
        granted: true,
        nodes: ['control-plane', 'worker-1'],
        reason: null,
      })
    )
    expect(description.state).toBe('granted')
    expect(description.detail).toBe('Granted on control-plane and worker-1.')
    expect(description.nodes).toEqual(['control-plane', 'worker-1'])
  })

  test('a single granting host is not joined with "and"', () => {
    expect(
      describeDockerSocket(
        capability({ granted: true, nodes: ['worker-1'], reason: null })
      ).detail
    ).toBe('Granted on worker-1.')
  })

  test('an ungranted project repeats the server reason verbatim', () => {
    const description = describeDockerSocket(capability())
    expect(description.state).toBe('not-granted')
    expect(description.detail).toContain('node-daemon')
  })

  test('an ungranted project with no reason still says something actionable', () => {
    expect(describeDockerSocket(capability({ reason: null })).detail).toBe(
      'No host grants this project access to the Docker socket.'
    )
  })

  test('a missing capability is unknown, never "not granted"', () => {
    // The list endpoints omit the field entirely; treating that as "not
    // granted" would show an onboarding prompt the server never asked for.
    expect(describeDockerSocket(null).state).toBe('unknown')
    expect(describeDockerSocket(undefined).state).toBe('unknown')
  })
})

describe('shouldShowDockerSocketOnboarding', () => {
  test('shows for an administrator when nothing grants the project', () => {
    expect(shouldShowDockerSocketOnboarding(capability(), true)).toBe(true)
  })

  test('never shows to a user who cannot manage nodes', () => {
    expect(shouldShowDockerSocketOnboarding(capability(), false)).toBe(false)
  })

  test('never shows when the project already has the grant', () => {
    expect(
      shouldShowDockerSocketOnboarding(
        capability({ granted: true, nodes: ['worker-1'], reason: null }),
        true
      )
    ).toBe(false)
  })

  test('never shows when the capability was not computed', () => {
    expect(shouldShowDockerSocketOnboarding(null, true)).toBe(false)
    expect(shouldShowDockerSocketOnboarding(undefined, true)).toBe(false)
  })
})

describe('shouldShowDockerSocketBadge', () => {
  test('only a granted capability earns the badge', () => {
    expect(
      shouldShowDockerSocketBadge(
        capability({ granted: true, nodes: ['worker-1'], reason: null })
      )
    ).toBe(true)
    expect(shouldShowDockerSocketBadge(capability())).toBe(false)
    expect(shouldShowDockerSocketBadge(null)).toBe(false)
  })
})
