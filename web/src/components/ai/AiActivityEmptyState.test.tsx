// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { AiActivityEmptyState } from './AiActivityEmptyState'

test('turns an empty AI activity view into a telemetry setup path', () => {
  const markup = renderToStaticMarkup(
    <MemoryRouter>
      <AiActivityEmptyState setupHref="/ai-gateway/setup" />
    </MemoryRouter>
  )

  expect(markup).toContain('No AI traces yet')
  expect(markup).toContain('model calls, token usage, and agent activity')
  expect(markup).toContain('href="/ai-gateway/setup"')
})
