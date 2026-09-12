// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { ContainerEmptyState } from './ContainerEmptyState'

test('links an empty environment to its project deployment history', () => {
  const markup = renderToStaticMarkup(
    <MemoryRouter>
      <ContainerEmptyState projectSlug="example-project" />
    </MemoryRouter>
  )

  expect(markup).toContain('Containers appear after a deployment starts')
  expect(markup).toContain('href="/projects/example-project/deployments"')
  expect(markup).toContain('View deployments')
})
