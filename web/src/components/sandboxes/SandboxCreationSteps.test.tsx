// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import type { CreationStep, SandboxCreation } from '@/api/client'
import { SandboxCreationSteps } from './SandboxCreationSteps'

function step(
  kind: CreationStep['kind'],
  title: string,
  overrides: Partial<CreationStep> = {}
): CreationStep {
  return {
    kind,
    title,
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

const T0 = '2026-01-01T00:00:00Z'
const T1 = '2026-01-01T00:00:01Z'
const T3 = '2026-01-01T00:00:03Z'

function creation(overrides: Partial<SandboxCreation> = {}): SandboxCreation {
  return {
    creation_id: 'create-0001',
    sandbox_id: 'sbx_test',
    status: 'running',
    started_at: T0,
    finished_at: null,
    error: null,
    steps: [
      step('check_request', 'Check the request', {
        status: 'done',
        started_at: T0,
        finished_at: T1,
      }),
      step('pull_image', 'Pull the sandbox image', {
        status: 'running',
        started_at: T1,
        detail: 'downloading',
        bytes_done: 1_200_000_000,
        bytes_total: 3_400_000_000,
      }),
      step('seed_source', 'Copy the source in', { status: 'skipped' }),
      step('ready', 'Ready'),
    ],
    ...overrides,
  }
}

const render = (c: SandboxCreation | undefined) =>
  renderToStaticMarkup(<SandboxCreationSteps creation={c} />)

describe('SandboxCreationSteps', () => {
  test('shows skeleton rows, not a spinner, before the first poll answers', () => {
    const html = render(undefined)
    expect(html).toContain('aria-busy="true"')
    expect(html.match(/animate-pulse/g)?.length).toBeGreaterThanOrEqual(6)
    expect(html).not.toContain('animate-spin')
  })

  test('lists every step in order with its status', () => {
    const html = render(creation())
    const order = [
      ...html.matchAll(/data-step="([a-z_]+)" data-status="([a-z]+)"/g),
    ].map((m) => `${m[1]}:${m[2]}`)
    expect(order).toEqual([
      'check_request:done',
      'pull_image:running',
      'seed_source:skipped',
      'ready:pending',
    ])
  })

  test('shows how long a finished step took', () => {
    expect(render(creation())).toContain('1.0s')
  })

  test('shows the download progress of the image step while it runs', () => {
    const html = render(creation())
    expect(html).toContain('1.2 GB of 3.4 GB')
    expect(html).toContain('role="progressbar"')
    expect(html).toContain('downloading')
  })

  test('on failure names the failed step and why', () => {
    const html = render(
      creation({
        status: 'failed',
        finished_at: T3,
        error: 'pulling image failed',
        steps: [
          step('check_request', 'Check the request', {
            status: 'done',
            started_at: T0,
            finished_at: T1,
          }),
          step('pull_image', 'Pull the sandbox image', {
            status: 'failed',
            started_at: T1,
            finished_at: T3,
            error: "pulling image 'img:1' failed: no space left on device",
          }),
          step('ready', 'Ready'),
        ],
      })
    )
    expect(html).toContain('data-step="pull_image" data-status="failed"')
    expect(html).toContain('no space left on device')
    expect(html).toContain('Failed at “Pull the sandbox image” after 3.0s.')
    expect(html).toContain('2.0s')
  })

  test('a finished create says how long it took overall', () => {
    const html = render(
      creation({
        status: 'succeeded',
        finished_at: T3,
        steps: [
          step('check_request', 'Check the request', {
            status: 'done',
            started_at: T0,
            finished_at: T1,
          }),
          step('ready', 'Ready', {
            status: 'done',
            started_at: T1,
            finished_at: T3,
          }),
        ],
      })
    )
    expect(html).toContain('Ready in 3.0s.')
    expect(html).not.toContain('animate-spin')
  })
})
