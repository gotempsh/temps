// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { act, cleanup, renderHook } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { TREE_FOLD_KEY, useTreeFold } from './use-tree-fold'

installBrowserWindow()
afterEach(() => {
  cleanup()
  window.localStorage.clear()
})

describe('useTreeFold', () => {
  test('an untouched row follows the page', () => {
    const { result } = renderHook(() => useTreeFold())
    expect(result.current.isOpen('tree:databases', true)).toBe(true)
    expect(result.current.isOpen('tree:databases', false)).toBe(false)
  })

  test('the choice holds on every page and across visits', () => {
    const first = renderHook(() => useTreeFold())
    act(() => first.result.current.setOpen('tree:domains', true))
    act(() => first.result.current.setOpen('tree:databases', false))
    expect(first.result.current.isOpen('tree:domains', false)).toBe(true)
    expect(first.result.current.isOpen('tree:databases', true)).toBe(false)
    expect(JSON.parse(window.localStorage.getItem(TREE_FOLD_KEY)!)).toEqual({
      'tree:domains': true,
      'tree:databases': false,
    })
    const next = renderHook(() => useTreeFold())
    expect(next.result.current.isOpen('tree:domains', false)).toBe(true)
    // Another row keeps its own choice.
    expect(next.result.current.isOpen('tree:project:3', false)).toBe(false)
  })

  test('a corrupt store reads as untouched', () => {
    window.localStorage.setItem(TREE_FOLD_KEY, 'nonsense')
    const { result } = renderHook(() => useTreeFold())
    expect(result.current.isOpen('tree:servers', true)).toBe(true)
  })
})
