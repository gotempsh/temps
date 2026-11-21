// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { LogIngestKeysBody } from './LogIngestKeys'

const renderBody = (projectId: number | undefined) =>
  renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <LogIngestKeysBody projectId={projectId} enabled={false} />
    </QueryClientProvider>
  )

test('without a service the body explains how to pick one', () => {
  const html = renderBody(undefined)
  expect(html).toContain('Keys belong to a service')
  expect(html).not.toContain('Create key')
})

test('with a service the body offers to create a key', () => {
  const html = renderBody(4)
  expect(html).toContain('Create key')
  expect(html).not.toContain('Keys belong to a service')
})
