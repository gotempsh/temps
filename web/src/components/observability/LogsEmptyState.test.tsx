// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { LogsEmptyState } from './LogsEmptyState'

test('offers telemetry setup from the first-use logs state', () => {
  const markup = renderToStaticMarkup(
    <MemoryRouter>
      <LogsEmptyState projectSlug="example-project" />
    </MemoryRouter>
  )

  expect(markup).toContain('No telemetry logs yet')
  expect(markup).toContain(
    'href="/projects/example-project/traces#traces-setup"'
  )
  expect(markup).toContain('Connect telemetry')
})
