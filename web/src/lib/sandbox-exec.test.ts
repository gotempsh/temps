// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { execFailureResult } from './sandbox-exec'

test('a problem document becomes its detail on stderr', () => {
  const result = execFailureResult({
    type: 'https://temps.sh/probs/sandbox-vm-not-running',
    title: 'Sandbox VM Not Running',
    detail: 'Sandbox sbx_1 exists, but its VM is not running',
  })
  expect(result).toEqual({
    exit_code: -1,
    stdout: '',
    stderr: 'Sandbox sbx_1 exists, but its VM is not running',
  })
})

test('a network error keeps its message', () => {
  expect(execFailureResult(new TypeError('Failed to fetch')).stderr).toBe(
    'Failed to fetch'
  )
})

test('an error with nothing to say still yields text, never undefined', () => {
  for (const error of [undefined, null, {}, 'boom']) {
    expect(typeof execFailureResult(error).stderr).toBe('string')
  }
})
