// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type { ServiceReadiness } from '@/api/client/types.gen'
import { ServiceReadinessPanel } from './ServiceReadinessPanel'

const STARTED_AT = '2026-10-09T10:00:00Z'
const noop = () => {}

function render(status: string, readiness?: ServiceReadiness | null) {
  return renderToStaticMarkup(
    <MemoryRouter>
      <ServiceReadinessPanel
        serviceId={7}
        status={status}
        readiness={readiness}
        onRetry={noop}
        onTryAnotherImage={noop}
        onRecreate={noop}
        now={Date.parse(STARTED_AT) + 15_000}
      />
    </MemoryRouter>
  )
}

const startingReadiness: ServiceReadiness = {
  phase: 'starting',
  started_at: STARTED_AT,
  deadline_secs: 180,
  reason:
    'rustfs probe to http://localhost:9000 ListBuckets failed with HTTP 503: RustFS is answering but its storage layer is not ready',
  restart_count: 2,
  failure: null,
}

const failedReadiness: ServiceReadiness = {
  ...startingReadiness,
  phase: 'failed',
  restart_count: 3,
  failure: {
    kind: 'store_init_failed',
    reason:
      'Initialization failed: the container logged a storage initialization error it does not recover from',
    log_excerpt: [
      'pool metadata recovery required: no durable bootstrap identity or pool.bin replica is available',
      'Server runtime failed: store init failed: init retry budget exhausted',
    ],
    next_actions: [
      'view_logs',
      'recreate_with_fresh_volumes',
      'try_another_image',
    ],
  },
}

describe('ServiceReadinessPanel', () => {
  it('holds a starting service in Starting with the probe reason and restarts', () => {
    const html = render('starting', startingReadiness)
    expect(html).toContain(
      'Starting: waiting for the service to accept requests'
    )
    expect(html).toContain('authenticated request with its own credentials')
    expect(html).toContain('Waiting 15s so far (gives up after 180s).')
    expect(html).toContain('HTTP 503')
    expect(html).toContain('restarted 2 time(s)')
    expect(html).not.toContain('Initialization failed')
  })

  it('shows the failure reason, log excerpt and next actions', () => {
    const html = render('failed', failedReadiness)
    expect(html).toContain('role="alert"')
    expect(html).toContain(
      'Initialization failed: Storage initialization failed'
    )
    expect(html).toContain('store init failed: init retry budget exhausted')
    expect(html).toContain('has not changed or deleted')
    expect(html).toContain('href="/storage/7/logs"')
    expect(html).toContain('Recreate with fresh volumes')
    expect(html).toContain('Delete service…')
    expect(html).toContain('Change image…')
    // Only the actions the backend suggested.
    expect(html).not.toContain('Start again')
  })

  it('offers a retry for a timeout', () => {
    const html = render('failed', {
      ...failedReadiness,
      failure: {
        kind: 'timeout',
        reason: 'Initialization did not finish within 180s',
        log_excerpt: [],
        next_actions: ['view_logs', 'retry', 'try_another_image'],
      },
    })
    expect(html).toContain('The service did not become ready in time')
    expect(html).toContain('Start again')
    expect(html).not.toContain('From the container logs')
  })

  it('renders nothing for a running service', () => {
    expect(render('running', null)).toBe('')
    expect(render('failed', null)).toBe('')
  })
})
