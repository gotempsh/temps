// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { platformToolGroups } from './platform-tools'

describe('platform tools AI discovery', () => {
  test('lists sandboxes but no longer the retired harness workspaces', () => {
    const items = platformToolGroups.flatMap((group) => group.items)
    expect(items.find((item) => item.title === 'Sandboxes')?.url).toBe(
      '/sandboxes'
    )
    expect(items.some((item) => item.url === '/workspaces')).toBe(false)
  })

  test('offers the assistant and AI providers, not retired AI surfaces', () => {
    const automate = platformToolGroups.find(
      (group) => group.label === 'Automate'
    )
    const byTitle = (title: string) =>
      automate?.items.find((item) => item.title === title)

    expect(byTitle('Temps assistant')?.url).toBe('/chat')
    expect(byTitle('Temps assistant')?.featureKey).toBe('ai-chat')
    expect(byTitle('AI providers')?.url).toBe('/ai-gateway')
    expect(byTitle('AI providers')?.featureKey).toBe('ai-gateway')
    // Connecting an external coding agent to Temps is unrelated to harnesses
    // running inside Temps, and stays.
    expect(byTitle('Connect AI harness')?.url).toBe('/setup/ai')
    const urls = platformToolGroups.flatMap((group) =>
      group.items.map((item) => item.url)
    )
    for (const retired of ['/ai-workflows', '/skills', '/mcp-servers']) {
      expect(urls).not.toContain(retired)
    }
  })
})

test('global observability is discoverable from platform tools', () => {
  const observe = platformToolGroups.find((group) => group.label === 'Observe')
  for (const path of ['/analytics', '/traces', '/logs', '/errors']) {
    expect(observe?.items.some((item) => item.url === path)).toBe(true)
  }
})

test('cross-project revenue is reachable from platform tools with its maturity badge', () => {
  const observe = platformToolGroups.find((group) => group.label === 'Observe')
  const revenue = observe?.items.find((item) => item.url === '/revenue')
  expect(revenue?.featureKey).toBe('revenue-tracking')
})
