// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { nodeCapabilityGetQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { SampleDeployCard } from './SampleDeployCard'

function render(canManageNodes: boolean): string {
  const client = new QueryClient()
  client.setQueryData(nodeCapabilityGetQueryKey(), {
    schedulable: false,
    can_manage_nodes: canManageNodes,
    setup_path: '/settings/nodes',
  })
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <SampleDeployCard />
      </MemoryRouter>
    </QueryClientProvider>
  )
  client.clear()
  return markup
}

describe('SampleDeployCard worker remedy', () => {
  test('links node managers to worker setup', () => {
    const markup = render(true)
    expect(markup).toContain('Add a worker node first')
    expect(markup).toContain('href="/settings/nodes"')
    expect(markup).not.toContain('Deploy sample app')
  })

  test('asks an administrator when the user cannot manage nodes', () => {
    const markup = render(false)
    expect(markup).toContain('Ask an administrator to add a worker node.')
    expect(markup).not.toContain('href="/settings/nodes"')
    expect(markup).not.toContain('Deploy sample app')
  })
})
