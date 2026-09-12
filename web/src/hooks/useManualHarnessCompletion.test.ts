// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  harnessCompletionKey,
  readHarnessCompletion,
} from './useManualHarnessCompletion'

test('manual completion persists without completing another account checklist', () => {
  const values = new Map([[harnessCompletionKey(1), 'true']])
  const storage = { getItem: (key: string) => values.get(key) ?? null }
  expect(readHarnessCompletion(storage, 1)).toBe(true)
  expect(readHarnessCompletion(storage, 2)).toBe(false)
})

test('missing, malformed, or blocked storage is not treated as completed', () => {
  for (const value of [null, 'false', 'yes', '{}']) {
    expect(readHarnessCompletion({ getItem: () => value }, 1)).toBe(false)
  }
  expect(
    readHarnessCompletion(
      {
        getItem: () => {
          throw new Error('blocked')
        },
      },
      1
    )
  ).toBe(false)
})
