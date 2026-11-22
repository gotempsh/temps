// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter, useLocation } from 'react-router'
import {
  adminListNodesOptions,
  nodeMetricsGetReportingOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { NodeInfoResponse } from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { scopeValue } from '@/lib/node-scope'
import { useMetricNodeScope } from './useMetricNodeScope'

installBrowserWindow()
afterEach(cleanup)

function Probe() {
  const nodes = useMetricNodeScope(true, 'first')
  const location = useLocation()
  return (
    <>
      <output>
        {scopeValue(nodes.scope)}|{location.search}
      </output>
      <button onClick={() => nodes.setScope('all')}>All nodes</button>
      <button onClick={() => nodes.setScope('7')}>Worker</button>
    </>
  )
}

function mount(search: string) {
  const client = new QueryClient()
  client.setQueryData(adminListNodesOptions().queryKey, {
    total: 1,
    nodes: [{ id: 7, name: 'worker', status: 'active' } as NodeInfoResponse],
  })
  client.setQueryData(nodeMetricsGetReportingOptions().queryKey, [])
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[`/server${search}`]}>
        <Probe />
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { view, client }
}

test('default and user choices persist in the URL without losing other parameters', async () => {
  const { view, client } = mount('?range=6h')
  try {
    await waitFor(() =>
      expect(view.getByText('0|?range=6h&node=0')).toBeTruthy()
    )
    fireEvent.click(view.getByText('All nodes'))
    await waitFor(() =>
      expect(view.getByText('all|?range=6h&node=all')).toBeTruthy()
    )
    fireEvent.click(view.getByText('Worker'))
    await waitFor(() =>
      expect(view.getByText('7|?range=6h&node=7')).toBeTruthy()
    )
  } finally {
    view.unmount()
    client.clear()
  }
})

test('reloading explicit worker and all-node links preserves the choice', async () => {
  for (const node of ['7', 'all']) {
    const { view, client } = mount(`?node=${node}`)
    try {
      await waitFor(() =>
        expect(view.getByText(`${node}|?node=${node}`)).toBeTruthy()
      )
    } finally {
      view.unmount()
      client.clear()
    }
  }
})
