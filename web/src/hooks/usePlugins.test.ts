// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { QueryClient, QueryObserver, focusManager } from '@tanstack/react-query'
import { pluginManifestQueryOptions } from './usePlugins'
import type { PluginManifest } from '@/types/plugins'

test('returning from a CLI install refreshes even fresh manifests without polling', async () => {
  const queries = new QueryClient()
  const options = pluginManifestQueryOptions()
  let requests = 0
  const observer = new QueryObserver(queries, {
    ...options,
    initialData: [] as PluginManifest[],
    queryFn: async () => {
      requests += 1
      return [] as PluginManifest[]
    },
  })
  focusManager.setFocused(false)
  queries.mount()
  const unsubscribe = observer.subscribe(() => {})
  try {
    expect(requests).toBe(0)
    expect(options.refetchOnWindowFocus).toBe('always')
    expect('refetchInterval' in options).toBe(false)
    focusManager.setFocused(true)
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(requests).toBe(1)
  } finally {
    unsubscribe()
    queries.unmount()
    queries.clear()
    focusManager.setFocused(undefined)
  }
})
