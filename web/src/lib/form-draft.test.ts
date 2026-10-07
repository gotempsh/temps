// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { beforeEach, describe, expect, test } from 'bun:test'
import {
  clearFormDraft,
  mergeFormDraft,
  readFormDraft,
  saveFormDraft,
} from './form-draft'

class MemoryStorage {
  private values = new Map<string, string>()
  getItem(key: string) {
    return this.values.get(key) ?? null
  }
  setItem(key: string, value: string) {
    this.values.set(key, value)
  }
  removeItem(key: string) {
    this.values.delete(key)
  }
}

describe('form drafts', () => {
  beforeEach(() => {
    Object.defineProperty(globalThis, 'window', {
      value: { sessionStorage: new MemoryStorage() },
      configurable: true,
    })
  })

  test('round-trips a draft until it is cleared', () => {
    saveFormDraft('alert-rule:7', { name: 'Spike', enabled: true })
    expect(readFormDraft('alert-rule:7')).toEqual({
      name: 'Spike',
      enabled: true,
    })
    expect(readFormDraft('alert-rule:8')).toBeNull()
    clearFormDraft('alert-rule:7')
    expect(readFormDraft('alert-rule:7')).toBeNull()
  })

  test('treats corrupt or unavailable storage as no draft', () => {
    window.sessionStorage.setItem('temps:form-draft:bad', '{not json')
    expect(readFormDraft('bad')).toBeNull()
    Object.defineProperty(globalThis, 'window', {
      value: {},
      configurable: true,
    })
    expect(() => saveFormDraft('x', 1)).not.toThrow()
    expect(readFormDraft('x')).toBeNull()
  })
})

describe('mergeFormDraft', () => {
  const base = {
    name: '',
    trigger_type: 'new_issue',
    trigger_config: {} as { count?: number; window_minutes?: number },
    cooldown_minutes: 60,
    environment_filter: null as number | null,
    label_filters: [] as { key: string; value: string }[],
    enabled: true,
  }

  test('keeps an unfinished draft that the submit schema would reject', () => {
    const merged = mergeFormDraft(base, {
      name: '',
      trigger_type: 'frequency',
      trigger_config: { count: 10, window_minutes: 5 },
      cooldown_minutes: 15,
      environment_filter: 3,
      label_filters: [{ key: 'route', value: '/checkout' }],
      enabled: false,
    })
    expect(merged).toEqual({
      name: '',
      trigger_type: 'frequency',
      trigger_config: { count: 10, window_minutes: 5 },
      cooldown_minutes: 15,
      environment_filter: 3,
      label_filters: [{ key: 'route', value: '/checkout' }],
      enabled: false,
    })
  })

  test('lays edits over a loaded rule', () => {
    const loaded = { ...base, name: 'Checkout errors', cooldown_minutes: 30 }
    expect(mergeFormDraft(loaded, { ...loaded, cooldown_minutes: 5 })).toEqual({
      ...loaded,
      cooldown_minutes: 5,
    })
  })

  test('keeps fields the user cleared while editing a saved rule', () => {
    const loaded = {
      ...base,
      name: 'Checkout errors',
      trigger_config: { count: 10, window_minutes: 5 },
      environment_filter: 3 as number | null,
    }
    // What getValues() serializes after clearing the environment filter and
    // the count: null for the select, and the emptied number dropped by JSON.
    const draft = JSON.parse(
      JSON.stringify({
        ...loaded,
        trigger_config: { count: undefined, window_minutes: 5 },
        environment_filter: null,
      })
    )
    const merged = mergeFormDraft(loaded, draft)
    expect(merged.environment_filter).toBeNull()
    expect(merged.trigger_config.count).toBeUndefined()
    expect(merged.trigger_config.window_minutes).toBe(5)
    expect(merged.name).toBe('Checkout errors')
  })

  test('ignores fields whose shape does not match the starting value', () => {
    const merged = mergeFormDraft(base, {
      ...base,
      cooldown_minutes: '15',
      enabled: 'yes',
      label_filters: 'route=/checkout',
      trigger_config: [1, 2],
      unexpected: { nested: true },
    })
    expect(merged).toEqual(base)
  })

  test('returns the starting values when there is no usable draft', () => {
    expect(mergeFormDraft(base, null)).toBe(base)
    expect(mergeFormDraft(base, 'draft')).toBe(base)
    expect(mergeFormDraft(base, [base])).toBe(base)
  })
})
