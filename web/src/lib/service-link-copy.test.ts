// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  advancedParamsHint,
  createAndLinkDescription,
  linkedResourceCopy,
  linkedResourceKind,
} from './service-link-copy'

function fullText(serviceType: string): string {
  const copy = linkedResourceCopy(serviceType)
  return [copy.lead, copy.example, copy.trail].filter(Boolean).join(' ')
}

describe('linkedResourceKind', () => {
  test('groups engines by what a link provisions', () => {
    expect(linkedResourceKind('postgres')).toBe('database')
    expect(linkedResourceKind('mariadb')).toBe('database')
    expect(linkedResourceKind('mongodb')).toBe('database')
    expect(linkedResourceKind('redis')).toBe('redis')
    expect(linkedResourceKind('kv')).toBe('redis')
    expect(linkedResourceKind('s3')).toBe('bucket')
    expect(linkedResourceKind('rustfs')).toBe('bucket')
    expect(linkedResourceKind('minio')).toBe('bucket')
    expect(linkedResourceKind('blob')).toBe('bucket')
    expect(linkedResourceKind('something-new')).toBe('other')
  })
})

describe('linkedResourceCopy', () => {
  test('SQL and document databases get a <project>_<env> database', () => {
    for (const type of ['postgres', 'mariadb', 'mongodb']) {
      expect(linkedResourceCopy(type).example).toBe('<project>_<env>')
      expect(fullText(type)).toContain('database')
    }
  })

  test('Redis never claims to create a named database', () => {
    const text = fullText('redis')
    expect(linkedResourceCopy('redis').example).toBeUndefined()
    expect(text).not.toContain('<project>_<env>')
    expect(text).toContain('logical database')
    expect(text).toContain('15')
  })

  test('object storage gets a <project>-<env> bucket', () => {
    for (const type of ['s3', 'rustfs', 'blob']) {
      expect(linkedResourceCopy(type).example).toBe('<project>-<env>')
      expect(fullText(type)).toContain('bucket')
      expect(fullText(type)).not.toContain('database')
    }
  })

  test('unknown engines make no resource claim', () => {
    expect(fullText('something-new')).not.toContain('database')
    expect(fullText('something-new')).not.toContain('bucket')
  })
})

describe('advancedParamsHint', () => {
  test('only database engines mention databases', () => {
    expect(advancedParamsHint('postgres')).toContain('database')
    expect(advancedParamsHint('redis')).toContain('password')
    expect(advancedParamsHint('redis')).not.toContain('per-project database')
    expect(advancedParamsHint('rustfs')).toContain('bucket')
    expect(advancedParamsHint('rustfs')).not.toContain('database')
  })
})

describe('createAndLinkDescription', () => {
  test('names the resource each engine provisions', () => {
    expect(createAndLinkDescription('postgres')).toContain(
      'configured naming strategy'
    )
    expect(createAndLinkDescription('redis')).toContain(
      'a Redis logical database'
    )
    expect(createAndLinkDescription('s3')).toContain('a bucket')
    expect(createAndLinkDescription('something-new')).not.toContain('database')
  })
})

test('database guidance allows configured shared strategies', () => {
  expect(fullText('postgres')).toContain(
    'Per-project and custom database strategies can share'
  )
  expect(createAndLinkDescription('postgres')).toContain(
    'per environment by default'
  )
})
