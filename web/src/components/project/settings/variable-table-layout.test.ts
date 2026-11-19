// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  variableTableColumnCount,
  variableTableLayout,
} from './variable-table-layout'

test('every column fits when there is room', () => {
  const layout = variableTableLayout(786)
  expect(layout).toEqual({ valueColumn: true, environmentsColumn: true })
  expect(variableTableColumnCount(layout)).toBe(6)
})

test('environments stack under the key first, at the boundary', () => {
  expect(variableTableLayout(680).environmentsColumn).toBe(true)
  const layout = variableTableLayout(679)
  expect(layout).toEqual({ valueColumn: true, environmentsColumn: false })
  expect(variableTableColumnCount(layout)).toBe(5)
})

test('on a phone the value stacks too, at the boundary', () => {
  expect(variableTableLayout(520).valueColumn).toBe(true)
  const layout = variableTableLayout(342)
  expect(layout).toEqual({ valueColumn: false, environmentsColumn: false })
  expect(variableTableColumnCount(layout)).toBe(4)
})

test('a width not measured yet keeps every column', () => {
  for (const width of [null, 0]) {
    expect(variableTableLayout(width)).toEqual({
      valueColumn: true,
      environmentsColumn: true,
    })
  }
})
