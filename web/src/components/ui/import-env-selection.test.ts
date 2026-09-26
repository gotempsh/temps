// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { resolveEnvironmentSelection } from './import-env-selection'

test('untouched environment selection follows asynchronously loaded options', () => {
  expect(resolveEnvironmentSelection(undefined, undefined)).toEqual([])
  expect(
    resolveEnvironmentSelection(undefined, [{ id: 1 }, { id: 2 }])
  ).toEqual([1, 2])
})

test('deselecting the last environment does not silently select all again', () => {
  expect(resolveEnvironmentSelection([], [{ id: 1 }])).toEqual([])
})

test('explicit selections survive option refresh and reset restores defaults', () => {
  const environments = [{ id: 1 }, { id: 2 }]
  expect(resolveEnvironmentSelection([2], environments)).toEqual([2])
  expect(resolveEnvironmentSelection(undefined, environments)).toEqual([1, 2])
})
