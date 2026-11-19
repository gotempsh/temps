// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type {
  CreationStep,
  HostImagePreparation,
  SandboxCreation,
} from '@/api/client'
import {
  anyHostPulling,
  creationRunning,
  downloadLabel,
  downloadPercent,
  failedStep,
  formatBytes,
  formatDuration,
  hostsToPrepare,
  imageStateLabel,
  newCreationId,
  stepDurationMs,
} from './sandbox-creation'

function step(overrides: Partial<CreationStep>): CreationStep {
  return {
    kind: 'pull_image',
    title: 'Pull the sandbox image',
    status: 'pending',
    detail: null,
    error: null,
    started_at: null,
    finished_at: null,
    bytes_done: null,
    bytes_total: null,
    ...overrides,
  }
}

function host(overrides: Partial<HostImagePreparation>): HostImagePreparation {
  return {
    node_id: 0,
    node_name: 'control plane',
    is_control_plane: true,
    eligible: true,
    ineligible_reason: null,
    keep_prepared: false,
    status: { image: 'img:1', state: 'not_pulled', already_present: false },
    status_error: null,
    prepared_image: null,
    stale_image: null,
    last_error: null,
    last_error_at: null,
    ...overrides,
  }
}

describe('creation ids', () => {
  test('are accepted by the server: 8 to 64 of [A-Za-z0-9_-]', () => {
    for (let i = 0; i < 20; i++) {
      expect(newCreationId()).toMatch(/^[A-Za-z0-9_-]{8,64}$/)
    }
  })
})

describe('step durations', () => {
  test('a finished step spans start to finish', () => {
    const s = step({
      status: 'done',
      started_at: '2026-01-01T00:00:00Z',
      finished_at: '2026-01-01T00:00:02.500Z',
    })
    expect(stepDurationMs(s, 0)).toBe(2500)
  })

  test('a running step counts up to now', () => {
    const s = step({ status: 'running', started_at: '2026-01-01T00:00:00Z' })
    expect(stepDurationMs(s, Date.parse('2026-01-01T00:01:05Z'))).toBe(65_000)
  })

  test('a pending step has none', () => {
    expect(stepDurationMs(step({}), Date.now())).toBeNull()
  })

  test('format reads naturally', () => {
    expect(formatDuration(400)).toBe('0.4s')
    expect(formatDuration(12_000)).toBe('12s')
    expect(formatDuration(185_000)).toBe('3m 05s')
  })
})

describe('download progress', () => {
  test('sizes and percent', () => {
    expect(formatBytes(512)).toBe('512 B')
    expect(formatBytes(3_400_000_000)).toBe('3.4 GB')
    expect(downloadPercent(500, 1000)).toBe(50)
    expect(downloadPercent(5, null)).toBeNull()
    expect(downloadLabel(1_200_000_000, 3_400_000_000)).toBe('1.2 GB of 3.4 GB')
    expect(downloadLabel(1_200_000_000, null)).toBe('1.2 GB')
    expect(downloadLabel(null, 10)).toBeNull()
  })
})

describe('creation state', () => {
  const base: SandboxCreation = {
    creation_id: 'create-0001',
    sandbox_id: null,
    status: 'running',
    steps: [step({ kind: 'check_request', status: 'done' }), step({})],
    error: null,
    started_at: '2026-01-01T00:00:00Z',
    finished_at: null,
  }

  test('polling goes on until the create finishes', () => {
    expect(creationRunning(undefined)).toBe(true)
    expect(creationRunning(base)).toBe(true)
    expect(creationRunning({ ...base, status: 'succeeded' })).toBe(false)
    expect(creationRunning({ ...base, status: 'failed' })).toBe(false)
  })

  test('the failed step is the one marked failed', () => {
    expect(failedStep(base)).toBeNull()
    const failed = {
      ...base,
      status: 'failed' as const,
      steps: [
        step({ kind: 'check_request', status: 'done' }),
        step({ status: 'failed', error: 'no space left on device' }),
      ],
    }
    expect(failedStep(failed)?.error).toBe('no space left on device')
  })
})

describe('host preparation', () => {
  test('state labels', () => {
    expect(imageStateLabel(null)).toBe('Unknown')
    expect(
      imageStateLabel({
        image: 'i',
        state: 'pulling',
        detail: 'building locally',
        already_present: false,
      })
    ).toBe('Building')
    expect(
      imageStateLabel({ image: 'i', state: 'ready', already_present: true })
    ).toBe('Ready')
  })

  test('only eligible hosts that still need the image are prepared', () => {
    const hosts = [
      host({ node_id: 0 }),
      host({
        node_id: 1,
        status: { image: 'i', state: 'ready', already_present: false },
      }),
      host({
        node_id: 2,
        status: { image: 'i', state: 'pulling', already_present: false },
      }),
      host({ node_id: 3, eligible: false, status: null }),
      host({
        node_id: 4,
        status: {
          image: 'i',
          state: 'failed',
          error: 'x',
          already_present: false,
        },
      }),
      host({
        node_id: 5,
        status: { image: 'i', state: 'unsupported', already_present: false },
      }),
    ]
    expect(hostsToPrepare(hosts).map((h) => h.node_id)).toEqual([0, 4])
    expect(anyHostPulling(hosts)).toBe(true)
    expect(anyHostPulling([hosts[0]])).toBe(false)
  })
})
