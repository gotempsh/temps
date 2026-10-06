// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { beforeEach, describe, expect, test } from 'bun:test'
import { clearFormDraft, readFormDraft, saveFormDraft } from './form-draft'

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
