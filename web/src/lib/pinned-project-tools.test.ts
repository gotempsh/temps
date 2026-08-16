import { describe, expect, test } from 'bun:test'
import {
  parsePinnedProjectTools,
  pinnedProjectToolsStorageKey,
} from './pinned-project-tools'

describe('pinned project tools', () => {
  test('scopes persistence to a project slug', () => {
    expect(pinnedProjectToolsStorageKey('api-control-plane')).toBe(
      'temps:pinned-project-tools:api-control-plane'
    )
  })

  test('keeps allowed URLs in the users chosen order without duplicates', () => {
    expect(
      parsePinnedProjectTools(
        JSON.stringify(['metrics', 'monitors', 'metrics', 'removed-tool']),
        ['metrics', 'monitors']
      )
    ).toEqual(['metrics', 'monitors'])
  })

  test('fails closed for malformed or unexpected stored values', () => {
    expect(parsePinnedProjectTools('{broken', ['metrics'])).toEqual([])
    expect(
      parsePinnedProjectTools(JSON.stringify({ tool: 'metrics' }), ['metrics'])
    ).toEqual([])
  })
})
