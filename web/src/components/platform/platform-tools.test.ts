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
