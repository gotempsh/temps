// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  applyPaste,
  generatedKeys,
  initialRows,
  isSecret,
  missingKeys,
  parseDotenv,
  publicWarning,
  typedVariables,
} from './env'

describe('env', () => {
  test('dotenv: comments, export, quotes and inline comments; the rest is reported', () => {
    const { vars, skipped } = parseDotenv(
      [
        '# comment',
        '',
        'export A=1',
        'B = "two words"',
        "C='x # y'",
        'D=4 # note',
        'not a line',
      ].join('\n')
    )
    expect(vars).toEqual([
      { key: 'A', value: '1' },
      { key: 'B', value: 'two words' },
      { key: 'C', value: 'x # y' },
      { key: 'D', value: '4' },
    ])
    expect(skipped).toEqual(['not a line'])
  })

  test('secrets by name, by shape, or a URL with a password; public prefixes warn', () => {
    expect(isSecret('STRIPE_SECRET_KEY')).toBe(true)
    expect(isSecret('LOG_LEVEL', 'info')).toBe(false)
    expect(isSecret('ANYTHING', 'postgres://u:p@h/db')).toBe(true)
    expect(isSecret('NEXT_PUBLIC_API_KEY')).toBe(false)
    expect(publicWarning('NEXT_PUBLIC_API_KEY')).toMatch(/every visitor/)
    expect(publicWarning('NEXT_PUBLIC_SITE_URL')).toBeNull()
  })

  test('rows: database names read-only, generated secrets, examples, and what is missing', () => {
    const rows = initialRows(
      [
        { key: 'DATABASE_URL', looks_secret: true },
        { key: 'SESSION_SECRET', looks_secret: true, can_generate: true },
        { key: 'LOG_LEVEL', example_value: 'info' },
        { key: 'STRIPE_SECRET_KEY', looks_secret: true },
      ],
      { label: 'PostgreSQL', names: ['DATABASE_URL'] }
    )
    expect(rows.map((r) => r.source)).toEqual([
      'database',
      'generate',
      'example',
      'missing',
    ])
    expect(missingKeys(rows)).toEqual(['STRIPE_SECRET_KEY'])
    expect(generatedKeys(rows)).toEqual(['SESSION_SECRET'])
    expect(typedVariables(rows)).toEqual([
      { key: 'LOG_LEVEL', value: 'info', is_secret: false },
    ])
  })

  test('a paste fills and adds rows, never a database one', () => {
    const rows = initialRows([{ key: 'DATABASE_URL' }, { key: 'API_TOKEN' }], {
      label: 'PostgreSQL',
      names: ['DATABASE_URL'],
    })
    const pasted = applyPaste(
      rows,
      'DATABASE_URL=postgres://x\nAPI_TOKEN=abc\nEXTRA=1\n???'
    )
    expect(pasted.applied).toBe(2)
    expect(pasted.skipped).toEqual(['???'])
    expect(pasted.rows.find((r) => r.key === 'DATABASE_URL')?.source).toBe(
      'database'
    )
    expect(typedVariables(pasted.rows)).toEqual([
      { key: 'API_TOKEN', value: 'abc', is_secret: true },
      { key: 'EXTRA', value: '1', is_secret: false },
    ])
    expect(missingKeys(pasted.rows)).toEqual([])
  })
})
