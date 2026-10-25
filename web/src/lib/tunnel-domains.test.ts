// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { QueryClient } from '@tanstack/react-query'
import { client } from '@/api/client/client.gen'
import type { ConnectorView } from '@/api/client/types.gen'
import { allTunnelsOptions, MAX_TUNNELS } from './tunnel-domains'

const original = client.getConfig()

afterEach(() => {
  client.setConfig({ baseUrl: original.baseUrl, fetch: original.fetch })
})

function tunnel(id: number): ConnectorView {
  return { id, name: `t${id}`, domain: `z${id}.example.com` } as ConnectorView
}

describe('allTunnelsOptions', () => {
  test('reads every page, not only the first', async () => {
    const all = Array.from({ length: 230 }, (_, i) => tunnel(i + 1))
    const pages: number[] = []
    client.setConfig({
      baseUrl: 'http://temps.test',
      fetch: (async (input: Request) => {
        const url = new URL(input.url)
        const page = Number(url.searchParams.get('page'))
        const size = Number(url.searchParams.get('page_size'))
        pages.push(page)
        const connectors = all.slice((page - 1) * size, page * size)
        return new Response(
          JSON.stringify({
            connectors,
            total: all.length,
            page,
            page_size: size,
          }),
          { headers: { 'Content-Type': 'application/json' } }
        )
      }) as typeof fetch,
    })

    const tunnels = await new QueryClient().fetchQuery(allTunnelsOptions())

    expect(pages).toEqual([1, 2, 3])
    expect(tunnels.map((t) => t.id)).toEqual(all.map((t) => t.id))
  })

  test('stops at the first page when there are more than it may load', async () => {
    const pages: number[] = []
    client.setConfig({
      baseUrl: 'http://temps.test',
      fetch: (async (input: Request) => {
        const url = new URL(input.url)
        const page = Number(url.searchParams.get('page'))
        const size = Number(url.searchParams.get('page_size'))
        pages.push(page)
        return new Response(
          JSON.stringify({
            connectors: Array.from({ length: size }, (_, i) =>
              tunnel((page - 1) * size + i + 1)
            ),
            total: MAX_TUNNELS + 1,
            page,
            page_size: size,
          }),
          { headers: { 'Content-Type': 'application/json' } }
        )
      }) as typeof fetch,
    })

    await expect(
      new QueryClient().fetchQuery(allTunnelsOptions())
    ).rejects.toThrow(/more than the 500/)
    expect(pages).toEqual([1])
  })
})
