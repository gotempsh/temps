// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { QueryContent } from './GlobalPage'

function render(props: {
  loading?: boolean
  error?: unknown
  empty?: boolean
}) {
  return renderToStaticMarkup(
    <MemoryRouter>
      <QueryContent
        title="Logs"
        loading={props.loading ?? false}
        error={props.error ?? null}
        empty={props.empty ?? false}
        retry={() => {}}
      >
        <p>row data</p>
      </QueryContent>
    </MemoryRouter>
  )
}

test('an empty result uses the compact empty state with a next action', () => {
  const html = render({ empty: true })
  expect(html).toContain('No logs in this view')
  expect(html).toContain('Choose another service or time range')
  expect(html).toContain('href="/projects"')
  expect(html).toContain('Open projects')
  expect(html).toContain('min-h-60')
  expect(html).not.toContain('border-dashed')
  expect(html).not.toContain('row data')
})

test('a failed query is an error with retry, never the empty state', () => {
  const html = render({ error: new Error('boom'), empty: true })
  expect(html).toContain('Logs could not be loaded')
  expect(html).toContain('Retry logs')
  expect(html).not.toContain('No logs in this view')
})

test('a loading query shows skeletons, not the empty state', () => {
  const html = render({ loading: true, empty: true })
  expect(html).toContain('aria-label="Loading logs"')
  expect(html).not.toContain('No logs in this view')
})

test('data renders its children', () => {
  const html = render({})
  expect(html).toContain('row data')
  expect(html).not.toContain('No logs in this view')
})
