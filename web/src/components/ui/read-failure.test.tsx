// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { ReadFailure } from './read-failure'

function render(error: unknown) {
  return renderToStaticMarkup(
    <ReadFailure
      resource="API keys"
      error={error}
      onRetry={() => {}}
      retrying={false}
    />
  )
}

test('a forbidden read renders a distinct access-denied state', () => {
  const html = render({
    title: 'Forbidden',
    status: 403,
    detail: 'Requires api_keys:read permission',
  })
  expect(html).toContain('API keys: access denied')
  expect(html).toContain('data-read-failure="forbidden"')
  expect(html).toContain('permission to read')
  expect(html).toContain('Requires api_keys:read permission')
  expect(html).not.toContain('API keys unavailable')
})

test('a server error renders the failure state with the server detail', () => {
  const html = render({
    title: 'Internal Server Error',
    status: 500,
    detail: 'Database error: connection reset while listing API keys',
  })
  expect(html).toContain('API keys unavailable')
  expect(html).toContain('data-read-failure="failed"')
  expect(html).toContain('Database error: connection reset')
  expect(html).toContain('Retry')
})

test('a network error never shows the client-side exception text', () => {
  const html = render(new TypeError('Failed to fetch'))
  expect(html).toContain('API keys unavailable')
  expect(html).toContain('Could not contact Temps')
  expect(html).not.toContain('Failed to fetch')
  expect(html).not.toContain('Server response')
})

test('a malformed problem body still renders the recovery state', () => {
  const html = render({ title: 'Server error', status: 500, detail: 42 })
  expect(html).toContain('API keys unavailable')
  expect(html).toContain('Retry')
  expect(html).not.toContain('Server response')
})

test('the route-level report is one line with the reason and a retry', () => {
  const html = renderToStaticMarkup(
    <ReadFailure
      resource="Data on this page"
      error={{
        title: 'Bad Gateway',
        status: 502,
        detail: 'upstream returned nothing for request 7',
      }}
      onRetry={() => {}}
      routeLevel
    />
  )
  expect(html).toContain('role="alert"')
  expect(html).toContain('Data on this page unavailable')
  expect(html).toContain('upstream returned nothing for request 7')
  // The explanation is kept for hover and screen readers, not stacked below.
  expect(html).toContain('title="Could not contact Temps')
  expect(html).toContain('flex-1 truncate')
  expect(html).not.toContain('Server response:')
  expect(html).toContain('>Retry<')
})
