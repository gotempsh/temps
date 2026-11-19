// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient } from '@tanstack/react-query'
import { getSandboxImagePreparationQueryKey } from '@/api/client/@tanstack/react-query.gen'
import type {
  CreationStep,
  HostImagePreparation,
  SandboxCreation,
} from '@/api/client'
import {
  anyHostPulling,
  creationRunning,
  creationUntracked,
  downloadLabel,
  downloadPercent,
  failedStep,
  formatBytes,
  formatDuration,
  hostsToPrepare,
  imageStateLabel,
  keepPolling,
  keepPollingStartingSandbox,
  showStartingSandboxSteps,
  refreshSandboxImageStatus,
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
    stale_images: [],
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

  test('stops polling a create the server never tracked once its request ended', () => {
    const notFound = { status: 404, title: 'Not Found' }
    // Before any answer: keep trying while the request is in flight, even
    // over a 404 (the first read races the server registering the id).
    expect(keepPolling(undefined, null, false)).toBe(true)
    expect(keepPolling(undefined, notFound, false)).toBe(true)
    // The request ended and the server does not track it: stop.
    expect(keepPolling(undefined, notFound, true)).toBe(false)
    // A tracked create is followed until it finishes.
    expect(keepPolling(base, null, true)).toBe(true)
    expect(keepPolling({ ...base, status: 'failed' }, null, false)).toBe(false)
  })

  test('a 404 over steps read earlier stops polling: the server lost the create', () => {
    const notFound = { status: 404, title: 'Not Found' }
    expect(keepPolling(base, notFound, false)).toBe(false)
    expect(keepPolling(base, notFound, true)).toBe(false)
  })

  test('a network or server error keeps polling while the create is unknown or running', () => {
    const unavailable = { status: 503, title: 'Service Unavailable' }
    const offline = new TypeError('Failed to fetch')
    expect(keepPolling(undefined, unavailable, true)).toBe(true)
    expect(keepPolling(undefined, offline, true)).toBe(true)
    expect(keepPolling(base, unavailable, true)).toBe(true)
    expect(
      keepPolling({ ...base, status: 'succeeded' }, unavailable, true)
    ).toBe(false)
  })

  test('only a 404 means the server does not track the create', () => {
    expect(creationUntracked({ status: 404 })).toBe(true)
    expect(creationUntracked({ status: 503 })).toBe(false)
    expect(creationUntracked(new TypeError('Failed to fetch'))).toBe(false)
    expect(creationUntracked(null)).toBe(false)
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

describe('refreshSandboxImageStatus', () => {
  test('marks the image status the card shows as stale', async () => {
    const client = new QueryClient()
    const queryKey = getSandboxImagePreparationQueryKey()
    client.setQueryData(queryKey, { hosts: [], all_ready: true })
    expect(client.getQueryState(queryKey)?.isInvalidated).toBe(false)
    await refreshSandboxImageStatus(client)
    expect(client.getQueryState(queryKey)?.isInvalidated).toBe(true)
  })
})

describe('a starting sandbox on its detail page', () => {
  const running = { status: 'running' } as SandboxCreation
  const succeeded = { status: 'succeeded' } as SandboxCreation
  const notFound = { status: 404, title: 'Not Found' }
  const serverError = { status: 503, title: 'Service Unavailable' }
  const networkError = new TypeError('Failed to fetch')

  test('polls while it runs and stops once it ends or the sandbox started', () => {
    expect(keepPollingStartingSandbox(true, undefined, null)).toBe(true)
    expect(keepPollingStartingSandbox(true, running, null)).toBe(true)
    expect(keepPollingStartingSandbox(true, succeeded, null)).toBe(false)
    expect(keepPollingStartingSandbox(false, running, null)).toBe(false)
  })

  test('only a 404 stops the poll; network and server errors keep it going', () => {
    expect(keepPollingStartingSandbox(true, undefined, notFound)).toBe(false)
    expect(keepPollingStartingSandbox(true, undefined, serverError)).toBe(true)
    expect(keepPollingStartingSandbox(true, undefined, networkError)).toBe(true)
    expect(keepPollingStartingSandbox(true, running, serverError)).toBe(true)
  })

  test('the card is left out only for a confirmed 404 with nothing read', () => {
    expect(showStartingSandboxSteps(true, undefined, null)).toBe(true)
    expect(showStartingSandboxSteps(true, undefined, notFound)).toBe(false)
    expect(showStartingSandboxSteps(true, undefined, serverError)).toBe(true)
    expect(showStartingSandboxSteps(true, undefined, networkError)).toBe(true)
    expect(showStartingSandboxSteps(true, running, notFound)).toBe(true)
    expect(showStartingSandboxSteps(false, running, null)).toBe(false)
  })
})
