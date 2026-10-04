// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { JobLogNoticeBar, JobLogPlaceholder } from './JobLogStatus'

const noop = () => {}

function placeholder(
  body: Parameters<typeof JobLogPlaceholder>[0]['body'],
  jobStatus = 'failure',
  detail: string | null = null
) {
  return renderToStaticMarkup(
    <JobLogPlaceholder
      body={body}
      jobStatus={jobStatus}
      detail={detail}
      onRetry={noop}
    />
  )
}

describe('JobLogPlaceholder', () => {
  test('loading renders skeleton lines, not a spinner', () => {
    const html = placeholder('loading')
    expect(html).toContain('animate-pulse')
    expect(html).not.toContain('animate-spin')
  })

  test('each terminal state has its own explanation', () => {
    expect(placeholder('not-started', 'pending')).toContain(
      'hasn&#x27;t started yet'
    )
    expect(placeholder('no-output', 'success')).toContain(
      'finished without writing any log output'
    )
    expect(placeholder('no-output', 'cancelled')).toContain(
      'This stage was cancelled before it wrote any log output.'
    )
    expect(
      placeholder('gone', 'failure', 'Logs for job are no longer available')
    ).toContain('no longer available')
  })

  test('an error shows its detail and a retry action', () => {
    const html = placeholder('error', 'failure', 'Failed to read logs: EIO')
    expect(html).toContain('load the logs for this stage')
    expect(html).toContain('Failed to read logs: EIO')
    expect(html).toContain('Retry')
  })
})

describe('JobLogNoticeBar', () => {
  test('renders nothing when the transport is healthy', () => {
    expect(
      renderToStaticMarkup(<JobLogNoticeBar notice="none" onRetry={noop} />)
    ).toBe('')
  })

  test('announces the HTTP fallback while the socket is down', () => {
    const html = renderToStaticMarkup(
      <JobLogNoticeBar notice="polling" onRetry={noop} />
    )
    expect(html).toContain('role="status"')
    expect(html).toContain('Live stream unavailable')
  })

  test('offers a retry when refreshing fails', () => {
    const html = renderToStaticMarkup(
      <JobLogNoticeBar notice="refresh-failed" onRetry={noop} />
    )
    expect(html).toContain('refresh the logs')
    expect(html).toContain('Retry')
  })
})
