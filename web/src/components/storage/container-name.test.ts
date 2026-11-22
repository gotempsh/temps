// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  MAX_CONTAINER_NAME_LENGTH,
  containerNameError,
  containerNameSchema,
} from './container-name'

describe('containerNameError', () => {
  it('accepts an empty name, which restores the default', () => {
    expect(containerNameError('', 'redis-')).toBeNull()
    expect(containerNameError('   ', 'redis-')).toBeNull()
  })

  it('accepts a name with the required prefix', () => {
    expect(containerNameError('redis-temps-kv-staging', 'redis-')).toBeNull()
    expect(containerNameError(' rustfs-files_2 ', 'rustfs-')).toBeNull()
  })

  it('requires the prefix and something after it', () => {
    expect(containerNameError('temps-kv', 'redis-')).toContain('redis-')
    expect(containerNameError('redis-', 'redis-')).toContain('redis-')
    expect(containerNameError('redis-x', 'rustfs-')).toContain('rustfs-')
  })

  it('rejects characters Temps does not allow', () => {
    for (const name of ['redis-a b', 'redis-a.b', 'redis-a/b', 'redis-é']) {
      expect(containerNameError(name, 'redis-')).toContain('letters, digits')
    }
  })

  it('rejects names longer than a DNS label', () => {
    const name = `redis-${'a'.repeat(MAX_CONTAINER_NAME_LENGTH)}`
    expect(containerNameError(name, 'redis-')).toContain('at most 63')
    expect(
      containerNameError(`redis-${'a'.repeat(57)}`, 'redis-')
    ).toBeNull()
  })
})

describe('containerNameSchema', () => {
  it('surfaces the message on the field', () => {
    const result = containerNameSchema('redis-').safeParse({
      container_name: 'wrong',
    })
    expect(result.success).toBe(false)
    if (!result.success) {
      expect(result.error.issues[0]?.path).toEqual(['container_name'])
      expect(result.error.issues[0]?.message).toContain('redis-')
    }
  })

  it('passes a valid name', () => {
    expect(
      containerNameSchema('redis-').safeParse({
        container_name: 'redis-custom',
      }).success
    ).toBe(true)
  })
})
