// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { act, cleanup, renderHook } from '@testing-library/react'
import { useFormBaseRevision } from './useFormBaseRevision'

afterEach(cleanup)

test('a clean form follows the latest revision', () => {
  const { result, rerender } = renderHook(
    ({ revision }) => useFormBaseRevision(false, revision),
    { initialProps: { revision: 'r1' as string | undefined } }
  )
  expect(result.current.baseRevision).toBe('r1')
  rerender({ revision: 'r2' })
  expect(result.current.baseRevision).toBe('r2')
})

test('an edited form keeps the revision its edits started from until it adopts one or is clean again', () => {
  const { result, rerender } = renderHook(
    ({ dirty, revision }) => useFormBaseRevision(dirty, revision),
    { initialProps: { dirty: false, revision: 'r1' as string | undefined } }
  )
  rerender({ dirty: true, revision: 'r1' })
  // A background refetch while the operator edits.
  rerender({ dirty: true, revision: 'r2' })
  expect(result.current.baseRevision).toBe('r1')

  act(() => result.current.adopt('r2'))
  expect(result.current.baseRevision).toBe('r2')
  rerender({ dirty: true, revision: 'r3' })
  expect(result.current.baseRevision).toBe('r2')

  // Saved or reset: clean again, so it follows the read once more.
  rerender({ dirty: false, revision: 'r3' })
  expect(result.current.baseRevision).toBe('r3')
})
