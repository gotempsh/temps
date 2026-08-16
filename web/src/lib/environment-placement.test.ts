import { describe, expect, test } from 'bun:test'

import {
  targetLabelsToPayload,
  targetNodesToPayload,
} from './environment-placement'

describe('targetNodesToPayload', () => {
  test('keeps an empty list so saving clears an existing node pin', () => {
    const body = { target_nodes: targetNodesToPayload([]) }

    expect(JSON.stringify(body)).toBe('{"target_nodes":[]}')
  })

  test('preserves selected node ids', () => {
    expect(targetNodesToPayload([2, 7])).toEqual([2, 7])
  })

  test('keeps an empty label object so saving clears an existing label pin', () => {
    const body = { target_labels: targetLabelsToPayload({}) }

    expect(JSON.stringify(body)).toBe('{"target_labels":{}}')
  })
})
