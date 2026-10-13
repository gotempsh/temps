// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  chooseSourceUrl,
  describeOutcome,
  HIDDEN_SOURCE,
  maskConnectionString,
  parseTimeoutMinutes,
  replaceConfirmationProblem,
} from './import-data.js'

describe('chooseSourceUrl', () => {
  test('prefers an environment variable over the command line', () => {
    expect(
      chooseSourceUrl({ sourceUrlEnv: 'SRC', sourceUrl: 'postgres://u:p@h/db' }, true),
    ).toEqual({ kind: 'env', name: 'SRC' })
  })

  test('uses the flag, then a hidden prompt, and refuses when neither is possible', () => {
    expect(chooseSourceUrl({ sourceUrl: 'postgres://u:p@h/db' }, false)).toEqual({ kind: 'flag' })
    expect(chooseSourceUrl({}, true)).toEqual({ kind: 'prompt' })
    expect(chooseSourceUrl({}, false)).toEqual({ kind: 'missing' })
  })
})

describe('maskConnectionString', () => {
  test('hides user and password and keeps the rest', () => {
    expect(
      maskConnectionString('postgres://app:s3cr%40t@db.example.com:5432/shop?sslmode=require'),
    ).toBe('postgres://***:***@db.example.com:5432/shop?sslmode=require')
    expect(maskConnectionString('redis://:pw@cache.example.com/0')).toBe(
      'redis://***:***@cache.example.com/0',
    )
  })

  test('leaves strings without credentials alone', () => {
    expect(maskConnectionString('mongodb://h.example.com/app')).toBe('mongodb://h.example.com/app')
  })

  test('hides a password that was not percent-encoded', () => {
    expect(maskConnectionString('postgres://user:pa/ss@db.example.com/app')).toBe(
      'postgres://***:***@db.example.com/app',
    )
    expect(maskConnectionString('postgres://user:p?w#d@x@db.example.com:5432/app')).toBe(
      'postgres://***:***@db.example.com:5432/app',
    )
    for (const password of ['pa/ss', 'p?w#d@x']) {
      expect(maskConnectionString(`mysql://root:${password}@db.example.com/app`)).not.toContain(
        password,
      )
    }
  })

  test('hides credential options', () => {
    expect(
      maskConnectionString('postgres://db.example.com/app?password=hunter2&sslmode=require'),
    ).toBe('postgres://db.example.com/app?password=***&sslmode=require')
  })

  test('hides anything that is not a connection string', () => {
    expect(maskConnectionString('user:secret db.example.com')).toBe(HIDDEN_SOURCE)
    expect(maskConnectionString('')).toBe(HIDDEN_SOURCE)
  })
})

describe('replaceConfirmationProblem', () => {
  test('accepts no replace, or replace with the repeated name', () => {
    expect(replaceConfirmationProblem({ target: 'shop' })).toBeNull()
    expect(
      replaceConfirmationProblem({
        target: 'shop',
        replace: true,
        confirmTarget: 'shop',
      }),
    ).toBeNull()
    // Asked interactively later.
    expect(replaceConfirmationProblem({ target: 'shop', replace: true })).toBeNull()
  })

  test('refuses a mismatched name or a confirmation without replace', () => {
    expect(
      replaceConfirmationProblem({
        target: 'shop',
        replace: true,
        confirmTarget: 'SHOP',
      }),
    ).toContain("exactly ('shop')")
    expect(replaceConfirmationProblem({ target: 'shop', confirmTarget: 'shop' })).toContain(
      'only applies together with --replace',
    )
  })
})

describe('parseTimeoutMinutes', () => {
  test('passes through valid minutes and leaves the default to the server', () => {
    expect(parseTimeoutMinutes(undefined, 1440)).toBeUndefined()
    expect(parseTimeoutMinutes('90', 1440)).toBe(90)
  })

  test('explains values outside the bounds', () => {
    for (const bad of ['0', '1441', '1.5', 'ten']) {
      expect(parseTimeoutMinutes(bad, 1440)).toContain('between 1 and 1440')
    }
  })
})

describe('describeOutcome', () => {
  test('summarises a success with the engine noun and size', () => {
    expect(
      describeOutcome(
        {
          status: 'succeeded',
          target_object_count: 3,
          target_size_bytes: 2048,
          error_message: null,
        },
        'table',
      ),
    ).toBe('Imported 3 tables, 2.0 KB')
    expect(
      describeOutcome(
        {
          status: 'succeeded',
          target_object_count: 1,
          target_size_bytes: null,
          error_message: null,
        },
        'collection',
      ),
    ).toBe('Imported 1 collection')
  })

  test('carries the reason of an unsuccessful run', () => {
    expect(
      describeOutcome({
        status: 'failed',
        target_object_count: null,
        target_size_bytes: null,
        error_message: 'Reading the source database failed: the source rejected the password.',
      }),
    ).toBe('Failed: Reading the source database failed: the source rejected the password.')
  })
})
