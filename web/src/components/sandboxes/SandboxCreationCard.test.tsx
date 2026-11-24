// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import type { SandboxCreation } from '@/api/client'
import { SandboxCreationCard } from './SandboxCreationCard'

const creation: SandboxCreation = {
  creation_id: 'create-0001',
  sandbox_id: 'sbx_test',
  status: 'running',
  started_at: '2026-01-01T00:00:00Z',
  finished_at: null,
  error: null,
  steps: [
    {
      kind: 'pull_image',
      title: 'Download the sandbox image',
      status: 'running',
      detail: 'downloading',
      error: null,
      started_at: '2026-01-01T00:00:00Z',
      finished_at: null,
      bytes_done: 50,
      bytes_total: 100,
    },
  ],
}

function render(
  refreshError: string | null,
  current: SandboxCreation | undefined
) {
  return renderToStaticMarkup(
    <SandboxCreationCard
      creation={current}
      refreshError={refreshError}
      onRetry={() => {}}
      retrying={false}
    />
  )
}

describe('SandboxCreationCard', () => {
  test('a failed refresh keeps the steps and download progress, with the error and a retry', () => {
    const html = render('network down', creation)
    expect(html).toContain('The steps could not be refreshed: network down')
    expect(html).toContain('Try again')
    expect(html).toContain('Download the sandbox image')
    expect(html).toContain('role="progressbar"')
  })

  test('a failed first read shows the error and a retry, not a loading skeleton', () => {
    const html = render('the server did not answer', undefined)
    expect(html).toContain(
      'The steps could not be read: the server did not answer'
    )
    expect(html).toContain('Try again')
    expect(html).not.toContain('aria-busy="true"')
  })

  test('no error, no alert', () => {
    const html = render(null, creation)
    expect(html).not.toContain('could not be refreshed')
    expect(html).toContain('Download the sandbox image')
  })
})
