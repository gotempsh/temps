// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import type { ServiceReadiness } from '@/api/client/types.gen'
import {
  isServiceSettling,
  readinessView,
  startingProgress,
} from './service-readiness'

const STARTED_AT = '2026-10-09T10:00:00Z'
const T_PLUS_42S = Date.parse(STARTED_AT) + 42_000

const starting = (
  overrides: Partial<ServiceReadiness> = {}
): ServiceReadiness => ({
  phase: 'starting',
  started_at: STARTED_AT,
  deadline_secs: 180,
  reason: null,
  restart_count: 0,
  failure: null,
  ...overrides,
})

const failed: ServiceReadiness = starting({
  phase: 'failed',
  restart_count: 4,
  reason:
    'rustfs probe to http://localhost:9000 ListBuckets failed with HTTP 503',
  failure: {
    kind: 'restart_loop',
    reason: 'Initialization failed: the container restarted 4 time(s)',
    log_excerpt: ['store init failed: init retry budget exhausted'],
    next_actions: [
      'view_logs',
      'try_another_image',
      'recreate_with_fresh_volumes',
    ],
  },
})

describe('readiness view', () => {
  it('reports a starting service with its latest probe reason and progress', () => {
    const view = readinessView(
      {
        status: 'starting',
        readiness: starting({
          reason: 'ListBuckets failed with HTTP 503',
          restart_count: 1,
        }),
      },
      T_PLUS_42S
    )
    expect(view).toEqual({
      kind: 'starting',
      reason: 'ListBuckets failed with HTTP 503',
      restartCount: 1,
      elapsedSecs: 42,
      deadlineSecs: 180,
    })
    if (view.kind !== 'starting') throw new Error('unreachable')
    expect(startingProgress(view)).toBe(
      'Waiting 42s so far (gives up after 180s).'
    )
  })

  it('still reports starting before the first readiness snapshot exists', () => {
    const view = readinessView({ status: 'starting' })
    expect(view.kind).toBe('starting')
    if (view.kind !== 'starting') throw new Error('unreachable')
    expect(startingProgress(view)).toBe(
      'Waiting for the first successful request.'
    )
  })

  it('reports an initialization failure with its typed reason', () => {
    const view = readinessView({ status: 'failed', readiness: failed })
    expect(view).toEqual({
      kind: 'failed',
      failure: failed.failure!,
      restartCount: 4,
    })
  })

  it('shows nothing for a running service or a failure without readiness', () => {
    expect(readinessView({ status: 'running', readiness: failed })).toEqual({
      kind: 'none',
    })
    expect(readinessView({ status: 'failed', readiness: null })).toEqual({
      kind: 'none',
    })
    expect(readinessView({ status: 'stopped' })).toEqual({ kind: 'none' })
  })

  it('keeps polling only while a service is creating or starting', () => {
    expect(isServiceSettling('starting')).toBe(true)
    expect(isServiceSettling('creating')).toBe(true)
    expect(isServiceSettling('running')).toBe(false)
    expect(isServiceSettling('failed')).toBe(false)
    expect(isServiceSettling(undefined)).toBe(false)
  })
})
