// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, test } from 'bun:test'

import { installBrowserWindow } from '@/test/dom'
import { openExternal } from './open-external'

installBrowserWindow()

describe('openExternal', () => {
  let calls: unknown[][]
  let original: typeof window.open

  beforeEach(() => {
    calls = []
    original = window.open
    window.open = ((...args: unknown[]) => {
      calls.push(args)
      return null
    }) as typeof window.open
  })

  afterEach(() => {
    window.open = original
  })

  test('opens the page in a new tab', () => {
    openExternal('https://github.com/settings/apps/new')

    expect(calls).toHaveLength(1)
    expect(calls[0][0]).toBe('https://github.com/settings/apps/new')
    expect(calls[0][1]).toBe('_blank')
  })

  test('gives the new page no reference back to the console', () => {
    openExternal('https://gitlab.example.com/-/user_settings/applications')

    const features = String(calls[0][2]).split(',')
    expect(features).toContain('noopener')
    expect(features).toContain('noreferrer')
  })
})
