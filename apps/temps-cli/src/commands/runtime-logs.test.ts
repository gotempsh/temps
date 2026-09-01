// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { selectRuntimeLogContainer } from './runtime-logs.js'

const containers = [
  { container_id: 'abc123456789', container_name: 'checkout-production' },
  { container_id: 'def987654321', container_name: 'worker-production' },
]

describe('selectRuntimeLogContainer', () => {
  test('defaults to the first live container', () => {
    expect(selectRuntimeLogContainer(containers)).toEqual(containers[0])
  })

  test('accepts partial IDs and names', () => {
    expect(selectRuntimeLogContainer(containers, 'def987')).toEqual(containers[1])
    expect(selectRuntimeLogContainer(containers, 'checkout')).toEqual(containers[0])
  })

  test('does not silently select a different container', () => {
    expect(selectRuntimeLogContainer(containers, 'missing')).toBeUndefined()
  })
})
