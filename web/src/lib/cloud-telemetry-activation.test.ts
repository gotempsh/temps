// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import type {
  BulkActivationJobProjectResponse,
  BulkActivationJobResponse,
} from '@/api/client/types.gen'
import {
  etaLabel,
  isInternalConsolePath,
  isJobActive,
  isTerminalJobStatus,
  percentLabel,
  problemStatus,
  problemValue,
  resolveConsoleProjectPath,
  resumableProjectIds,
  retryableProjectIds,
  skipReasonText,
  throughputLabel,
} from '@/lib/cloud-telemetry-activation'

function projectRow(
  overrides: Partial<BulkActivationJobProjectResponse> & { project_id: number }
): BulkActivationJobProjectResponse {
  return {
    bytes_shipped: 0,
    estimated_bytes: 0,
    estimated_spans: 0,
    spans_shipped: 0,
    status: 'pending',
    window_from: '2026-01-01T00:00:00Z',
    window_to: '2026-02-01T00:00:00Z',
    ...overrides,
  }
}

function job(
  projects: BulkActivationJobProjectResponse[],
  overrides: Partial<BulkActivationJobResponse> = {}
): BulkActivationJobResponse {
  return {
    batch_id: '00000000-0000-0000-0000-000000000000',
    bytes_shipped: 0,
    cancel_requested: false,
    created_at: '2026-01-01T00:00:00Z',
    estimated_bytes: 0,
    estimated_spans: 0,
    eta_state: 'estimating',
    projects,
    projects_done: 0,
    projects_failed: 0,
    projects_pending: projects.length,
    projects_skipped: 0,
    projects_total: projects.length,
    spans_shipped: 0,
    status: 'running',
    trigger: 'operator',
    ...overrides,
  }
}

describe('job lifecycle', () => {
  test('every terminal status stops polling', () => {
    expect(isTerminalJobStatus('completed')).toBe(true)
    expect(isTerminalJobStatus('completed_with_failures')).toBe(true)
    expect(isTerminalJobStatus('aborted')).toBe(true)
    expect(isTerminalJobStatus('cancelled')).toBe(true)
    expect(isTerminalJobStatus('pending')).toBe(false)
    expect(isTerminalJobStatus('running')).toBe(false)
  })

  test('a missing job is not an active job', () => {
    expect(isJobActive(undefined)).toBe(false)
    expect(isJobActive(null)).toBe(false)
    expect(isJobActive(job([], { status: 'pending' }))).toBe(true)
  })
})

describe('etaLabel', () => {
  test('never renders a number while the rate is still being measured', () => {
    expect(etaLabel('estimating', undefined)).toBe('estimating…')
    // Even with a number present, `estimating` must not render it.
    expect(etaLabel('estimating', 3600)).toBe('estimating…')
  })

  test('renders nothing once the job has finished', () => {
    expect(etaLabel('finished', undefined)).toBeNull()
    expect(etaLabel('finished', 0)).toBeNull()
  })

  test('renders a coarse duration when the server says it knows', () => {
    expect(etaLabel('known', 45)).toBe('about a minute left')
    expect(etaLabel('known', 600)).toBe('about 10 minutes left')
    expect(etaLabel('known', 7200)).toBe('about 2 hours left')
    expect(etaLabel('known', 172_800)).toBe('about 2 days left')
  })

  test('falls back to honesty when `known` carries no number', () => {
    expect(etaLabel('known', null)).toBe('estimating…')
  })
})

describe('percentLabel', () => {
  test('an omitted percentage is a dash, never a stalled 0%', () => {
    expect(percentLabel(undefined)).toBe('—')
    expect(percentLabel(null)).toBe('—')
  })

  test('a real zero is still rendered as zero', () => {
    expect(percentLabel(0)).toBe('0.0%')
    expect(percentLabel(42.4)).toBe('42%')
  })
})

describe('throughputLabel', () => {
  test('no measured rate produces no claim', () => {
    expect(throughputLabel(undefined)).toBeNull()
    expect(throughputLabel(null)).toBeNull()
    expect(throughputLabel(0)).toBeNull()
  })

  test('sub-second rates are rendered per minute so they are readable', () => {
    expect(throughputLabel(0.5)).toBe('30 spans/min')
    expect(throughputLabel(1200)).toBe('1,200 spans/s')
  })
})

describe('skipReasonText', () => {
  test('renders the server sentence verbatim', () => {
    const row = projectRow({
      project_id: 1,
      status: 'skipped',
      skip_reason: 'fidelity_not_queryable',
      skip_detail:
        'Telemetry fidelity is metered, so nothing readable would reach Cloud.',
    })
    expect(skipReasonText(row)).toBe(
      'Telemetry fidelity is metered, so nothing readable would reach Cloud.'
    )
  })

  test('falls back to the raw token rather than inventing prose', () => {
    const row = projectRow({
      project_id: 1,
      status: 'skipped',
      skip_reason: 'some_future_reason',
    })
    expect(skipReasonText(row)).toBe('some_future_reason')
  })
})

describe('retry and resume scoping', () => {
  const finished = job(
    [
      projectRow({ project_id: 1, status: 'done' }),
      projectRow({ project_id: 2, status: 'failed' }),
      projectRow({
        project_id: 3,
        status: 'skipped',
        skip_reason: 'fidelity_not_queryable',
      }),
      projectRow({
        project_id: 4,
        status: 'skipped',
        skip_reason: 'project_not_found',
      }),
    ],
    { status: 'completed_with_failures' }
  )

  test('retry covers failures and fixable skips, never a deleted project', () => {
    expect(retryableProjectIds(finished)).toEqual([2, 3])
  })

  test('resume covers untouched and interrupted work, never completed work', () => {
    const aborted = job(
      [
        projectRow({ project_id: 1, status: 'done' }),
        projectRow({ project_id: 2, status: 'backfilling' }),
        projectRow({ project_id: 3, status: 'pending' }),
        projectRow({ project_id: 4, status: 'skipped' }),
        projectRow({ project_id: 5, status: 'failed' }),
      ],
      { status: 'aborted' }
    )
    expect(resumableProjectIds(aborted)).toEqual([2, 3, 5])
  })
})

describe('problem bodies', () => {
  test('reads the values the server attached to a 409/400', () => {
    const conflict = {
      status: 409,
      batch_id: 'abc',
      status_path: '/otel/cloud-telemetry/bulk-jobs/abc',
    }
    expect(problemStatus(conflict)).toBe(409)
    expect(problemValue(conflict, 'batch_id')).toBe('abc')
    expect(problemValue(conflict, 'missing')).toBeUndefined()
  })

  test('tolerates a non-object rejection', () => {
    expect(problemStatus('boom')).toBeUndefined()
    expect(problemValue(null, 'batch_id')).toBeUndefined()
  })
})

describe('console paths', () => {
  test('accepts only same-document absolute paths', () => {
    expect(isInternalConsolePath('/settings/cloud')).toBe(true)
    expect(isInternalConsolePath('https://example.invalid')).toBe(false)
    expect(isInternalConsolePath('//example.invalid')).toBe(false)
    expect(isInternalConsolePath('/\\example.invalid')).toBe(false)
    expect(isInternalConsolePath('/settings/ cloud')).toBe(false)
    expect(isInternalConsolePath(undefined)).toBe(false)
    expect(isInternalConsolePath('')).toBe(false)
  })

  test('rewrites an id-based project path onto the slug the router expects', () => {
    const slugs = new Map([[7, 'checkout-api']])
    expect(
      resolveConsoleProjectPath('/projects/7/settings/telemetry', slugs)
    ).toBe('/projects/checkout-api/settings/telemetry')
    // Unknown project: leave the server's path alone rather than guess.
    expect(resolveConsoleProjectPath('/projects/9/settings', slugs)).toBe(
      '/projects/9/settings'
    )
    // Not the shape we know about: untouched.
    expect(resolveConsoleProjectPath('/settings/cloud', slugs)).toBe(
      '/settings/cloud'
    )
  })
})
