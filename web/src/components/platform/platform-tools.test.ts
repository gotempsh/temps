// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { platformToolGroups } from './platform-tools'

describe('platform tools AI discovery', () => {
  test('keeps harness setup discoverable without conflating built-in AI', () => {
    const automate = platformToolGroups.find(
      (group) => group.label === 'Automate'
    )

    expect(
      automate?.items.find((item) => item.title === 'Connect AI harness')?.url
    ).toBe('/setup/ai')
    expect(
      automate?.items.find((item) => item.title === 'Built-in AI')?.url
    ).toBe('/ai-gateway')
  })
})

test('global observability is discoverable from platform tools', () => {
  const observe = platformToolGroups.find((group) => group.label === 'Observe')
  for (const path of ['/analytics', '/traces', '/logs', '/errors']) {
    expect(observe?.items.some((item) => item.url === path)).toBe(true)
  }
})
