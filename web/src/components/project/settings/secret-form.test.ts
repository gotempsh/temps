// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  composeServiceNamesFor,
  MAX_SECRET_BYTES,
  secretEditChangeScope,
  secretEditDefaults,
  secretEditHasChanges,
  secretEditSchema,
  secretEditToRequest,
} from './secret-form'

const stored = {
  environments: [
    { id: 3, name: 'production', main_url: 'https://app.example.test' },
    { id: 5, name: 'staging', main_url: 'https://staging.example.test' },
  ],
  include_in_preview: true,
  compose_services: ['web'],
}

describe('secret edit defaults', () => {
  test('mirror the stored scope and never carry a value', () => {
    expect(secretEditDefaults(stored)).toEqual({
      value: '',
      environment_ids: [3, 5],
      include_in_preview: true,
      compose_services: ['web'],
    })
  })

  test('a secret without compose services gets an empty list', () => {
    const { compose_services: _omitted, ...rest } = stored
    expect(
      secretEditDefaults(rest as unknown as typeof stored).compose_services
    ).toEqual([])
  })

  test('no selected secret gives the empty, unscoped form', () => {
    expect(secretEditDefaults(null)).toEqual({
      value: '',
      environment_ids: [],
      include_in_preview: false,
      compose_services: [],
    })
  })
})

describe('secret edit payload', () => {
  test('a blank value is omitted so the stored value is kept', () => {
    const body = secretEditToRequest(secretEditDefaults(stored))
    expect('value' in body).toBe(false)
    expect(body).toEqual({
      environment_ids: [3, 5],
      include_in_preview: true,
      compose_services: ['web'],
    })
  })

  test('a typed value is sent verbatim, whitespace included', () => {
    const body = secretEditToRequest({
      ...secretEditDefaults(stored),
      value: '  new-secret\n',
    })
    expect(body.value).toBe('  new-secret\n')
  })

  test('scope is always sent in full so it is replaced, not merged', () => {
    const body = secretEditToRequest({
      ...secretEditDefaults(stored),
      environment_ids: [],
      include_in_preview: false,
      compose_services: [],
    })
    expect(body.environment_ids).toEqual([])
    expect(body.include_in_preview).toBe(false)
    expect(body.compose_services).toEqual([])
  })

  test('does not alias the form arrays', () => {
    const values = secretEditDefaults(stored)
    const body = secretEditToRequest(values)
    body.environment_ids?.push(99)
    expect(values.environment_ids).toEqual([3, 5])
  })
})

describe('secret edit validation', () => {
  test('accepts a blank value (keep current)', () => {
    expect(secretEditSchema.safeParse(secretEditDefaults(stored)).success).toBe(
      true
    )
  })

  test('accepts a value at the byte limit', () => {
    const result = secretEditSchema.safeParse({
      ...secretEditDefaults(stored),
      value: 'a'.repeat(MAX_SECRET_BYTES),
    })
    expect(result.success).toBe(true)
  })

  test('rejects a value over the limit, counting bytes not characters', () => {
    // Each of these is two bytes in UTF-8, so half the limit in characters
    // plus one is already over.
    const over = 'é'.repeat(MAX_SECRET_BYTES / 2 + 1)
    const result = secretEditSchema.safeParse({
      ...secretEditDefaults(stored),
      value: over,
    })
    expect(result.success).toBe(false)
    if (!result.success) {
      expect(result.error.issues[0]?.path).toEqual(['value'])
      expect(result.error.issues[0]?.message).toContain('1 MiB')
    }
  })

  test('rejects non-integer environment ids', () => {
    const result = secretEditSchema.safeParse({
      ...secretEditDefaults(stored),
      environment_ids: [1.5],
    })
    expect(result.success).toBe(false)
  })
})

describe('secretEditHasChanges', () => {
  test('is false for an untouched form', () => {
    expect(secretEditHasChanges(secretEditDefaults(stored), stored)).toBe(false)
  })

  test('is false when no secret is selected', () => {
    expect(secretEditHasChanges(secretEditDefaults(null), null)).toBe(false)
  })

  test('is true once a value is entered', () => {
    expect(
      secretEditHasChanges(
        { ...secretEditDefaults(stored), value: 'rotated' },
        stored
      )
    ).toBe(true)
  })

  test('ignores the order of scope lists', () => {
    expect(
      secretEditHasChanges(
        { ...secretEditDefaults(stored), environment_ids: [5, 3] },
        stored
      )
    ).toBe(false)
  })

  test('is true when scope changes', () => {
    const base = secretEditDefaults(stored)
    expect(
      secretEditHasChanges({ ...base, environment_ids: [3] }, stored)
    ).toBe(true)
    expect(
      secretEditHasChanges({ ...base, include_in_preview: false }, stored)
    ).toBe(true)
    expect(
      secretEditHasChanges({ ...base, compose_services: [] }, stored)
    ).toBe(true)
  })
})

describe('secretEditChangeScope', () => {
  test('is the union of the environments before and after', () => {
    expect(secretEditChangeScope([1, 2], [2, 3])).toEqual([1, 2, 3])
  })

  test('an unscoped secret is mounted everywhere, before or after', () => {
    expect(secretEditChangeScope([], [4])).toBe('all')
    expect(secretEditChangeScope([4], [])).toBe('all')
  })
})

describe('composeServiceNamesFor', () => {
  test('lists synced service names once, sorted', () => {
    expect(
      composeServiceNamesFor({
        preset_config: {
          composeServices: [{ name: 'web' }, { name: 'db' }, { name: 'web' }],
        },
      })
    ).toEqual(['db', 'web'])
  })

  test('is empty for presets without compose services', () => {
    expect(composeServiceNamesFor({ preset_config: null })).toEqual([])
    expect(composeServiceNamesFor({})).toEqual([])
  })
})
