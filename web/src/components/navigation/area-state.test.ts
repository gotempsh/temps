// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { installBrowserWindow } from '@/test/dom'
import {
  UNKNOWN,
  listsAsTab,
  rememberStates,
  rememberedStates,
  withRemembered,
} from './area-state'

installBrowserWindow()
afterEach(() => window.localStorage.clear())

describe('listsAsTab', () => {
  test('a view in use is a tab; one known to hold nothing is not', () => {
    expect(listsAsTab({ state: 'active' })).toBe(true)
    expect(listsAsTab({ state: 'inactive' })).toBe(false)
  })

  test('never hides a view on a guess', () => {
    expect(listsAsTab(UNKNOWN)).toBe(true)
    // A view with no set-up state is always a tab.
    expect(listsAsTab(undefined)).toBe(true)
  })
})

describe('remembered states', () => {
  test('keeps known states per scope and drops unknown ones', () => {
    rememberStates('server', {
      domains: { state: 'inactive' },
      databases: { state: 'active', metric: '2 databases' },
      email: UNKNOWN,
    })
    rememberStates('project:3', { domains: { state: 'active' } })
    expect(rememberedStates('server')).toEqual({
      domains: { state: 'inactive' },
      databases: { state: 'active' },
    })
    expect(rememberedStates('project:3')).toEqual({
      domains: { state: 'active' },
    })
    expect(rememberedStates('project:4')).toEqual({})
  })

  test('an unknown answer does not erase what was seen before', () => {
    rememberStates('server', { domains: { state: 'inactive' } })
    rememberStates('server', { domains: UNKNOWN })
    expect(rememberedStates('server').domains).toEqual({ state: 'inactive' })
  })

  test('a corrupt store reads as no memory', () => {
    window.localStorage.setItem('temps:nav:area-states', '{not json')
    expect(rememberedStates('server')).toEqual({})
  })
})

describe('withRemembered', () => {
  test('a live answer wins; an unknown one falls back to the last seen', () => {
    expect(
      withRemembered(
        {
          domains: UNKNOWN,
          databases: { state: 'active', metric: '2 databases' },
          email: UNKNOWN,
        },
        { domains: { state: 'inactive' }, databases: { state: 'inactive' } }
      )
    ).toEqual({
      domains: { state: 'inactive' },
      databases: { state: 'active', metric: '2 databases' },
      email: UNKNOWN,
    })
  })
})
