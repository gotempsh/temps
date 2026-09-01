// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import {
  schemaToPromptParams,
  parseSetPairs,
  buildExampleCommand,
  parseFromFlag,
  formatLogTs,
  truncateQuery,
  buildSlowQueriesErrorMessage,
  buildEnablePgStatStatementsErrorMessage,
  parsePositiveIntId,
  isValidComparator,
  isValidSeverity,
} from './index.js'

describe('schemaToPromptParams', () => {
  test('returns nothing for a schema with no properties', () => {
    expect(schemaToPromptParams({})).toEqual([])
  })

  test('marks fields present in required/readonly and title-cases the label', () => {
    const params = schemaToPromptParams({
      properties: {
        database_name: { type: 'string', default: 'app' },
        port: { type: 'integer' },
      },
      required: ['database_name'],
      readonly: ['port'],
    })

    const db = params.find((p) => p.name === 'database_name')!
    expect(db.label).toBe('Database Name')
    expect(db.required).toBe(true)
    expect(db.readonly).toBe(false)
    expect(db.default_value).toBe('app')

    const port = params.find((p) => p.name === 'port')!
    expect(port.required).toBe(false)
    expect(port.readonly).toBe(true)
  })

  test('falls back to example when no default is given', () => {
    const [param] = schemaToPromptParams({
      properties: { username: { type: 'string', example: 'postgres' } },
    })
    expect(param!.default_value).toBe('postgres')
  })
})

describe('parseSetPairs', () => {
  test('coerces recognised literals to their typed form', () => {
    expect(parseSetPairs(['a=true', 'b=false', 'c=42'])).toEqual({ a: true, b: false, c: 42 })
  })

  test('keeps a leading-zero numeric string as a string', () => {
    // "0123" as a Number would silently drop the leading zero — treat it as
    // an opaque string (e.g. a port or version fragment) instead.
    expect(parseSetPairs(['version=0123'])).toEqual({ version: '0123' })
  })

  test('treats a bare "0" as the number zero, not the leading-zero string case', () => {
    expect(parseSetPairs(['replicas=0'])).toEqual({ replicas: 0 })
  })

  test('keeps a non-numeric value as a plain string', () => {
    expect(parseSetPairs(['database=myapp'])).toEqual({ database: 'myapp' })
  })

  test('keeps an empty value as an empty string rather than coercing it', () => {
    expect(parseSetPairs(['password='])).toEqual({ password: '' })
  })

  test('rejects a pair with no "="', () => {
    expect(() => parseSetPairs(['justakey'])).toThrow('Expected format: key=value')
  })

  test('rejects a pair with an empty key', () => {
    expect(() => parseSetPairs(['=value'])).toThrow('Key cannot be empty')
  })

  test('later pairs win when the same key repeats', () => {
    expect(parseSetPairs(['a=1', 'a=2'])).toEqual({ a: 2 })
  })
})

describe('buildExampleCommand', () => {
  test('includes only schema fields that have a real default', () => {
    // password/port are typically auto-generated server-side (default null)
    // — showing them as --set flags would suggest the user must supply them.
    const cmd = buildExampleCommand('postgres', {
      properties: {
        database: { type: 'string', default: 'myapp' },
        password: { type: 'string', default: null },
        port: { type: 'integer' },
      },
    })
    expect(cmd).toBe('bunx @temps-sdk/cli services create -t postgres -n my-postgres --set database=myapp')
  })

  test('omits --set entirely when there is no schema', () => {
    expect(buildExampleCommand('redis')).toBe('bunx @temps-sdk/cli services create -t redis -n my-redis')
  })
})

describe('parseFromFlag', () => {
  test('parses minutes/hours/days into a past Date', () => {
    const now = Date.now()
    const d = parseFromFlag('2h')!
    expect(now - d.getTime()).toBeGreaterThanOrEqual(2 * 3_600_000 - 1000)
    expect(now - d.getTime()).toBeLessThan(2 * 3_600_000 + 5000)
  })

  test('returns null for a value that is not a recognised relative duration', () => {
    // Callers fall back to treating this as an ISO 8601 timestamp instead.
    expect(parseFromFlag('2024-01-01T00:00:00Z')).toBeNull()
    expect(parseFromFlag('2w')).toBeNull()
  })
})

describe('formatLogTs', () => {
  test('formats a valid ISO timestamp without the T separator or millis', () => {
    expect(formatLogTs('2024-03-01T12:34:56.789Z')).toBe('2024-03-01 12:34:56')
  })

  test('passes an unparsable timestamp through unchanged rather than printing "Invalid Date"', () => {
    expect(formatLogTs('not-a-date')).toBe('not-a-date')
  })
})

describe('truncateQuery', () => {
  test('collapses internal whitespace/newlines from a formatted query', () => {
    expect(truncateQuery('select  1\n  from   x')).toBe('select 1 from x')
  })

  test('leaves short queries untouched', () => {
    expect(truncateQuery('select 1')).toBe('select 1')
  })

  test('truncates long queries with an ellipsis at the configured length', () => {
    const long = 'select ' + 'a'.repeat(100)
    const out = truncateQuery(long, 20)
    expect(out.length).toBe(20)
    expect(out.endsWith('…')).toBe(true)
  })
})

describe('buildSlowQueriesErrorMessage', () => {
  test('adds setup instructions when the extension is not loaded', () => {
    const out = buildSlowQueriesErrorMessage('relation pg_stat_statements does not exist')
    expect(out).toContain('shared_preload_libraries')
    expect(out).toContain('pg_stat_statements does not exist')
  })

  test('passes other errors through untouched', () => {
    expect(buildSlowQueriesErrorMessage('connection refused')).toBe('connection refused')
  })
})

describe('buildEnablePgStatStatementsErrorMessage', () => {
  test('explains that clustered services need a manual rolling restart', () => {
    const out = buildEnablePgStatStatementsErrorMessage('service is part of a cluster')
    expect(out).toContain('rolling restart')
  })

  test('is case-insensitive when detecting the cluster error', () => {
    expect(buildEnablePgStatStatementsErrorMessage('Cluster mode active')).toContain('rolling restart')
  })

  test('passes other errors through untouched', () => {
    expect(buildEnablePgStatStatementsErrorMessage('permission denied')).toBe('permission denied')
  })
})

describe('parsePositiveIntId', () => {
  test('parses a positive integer string', () => {
    expect(parsePositiveIntId('42')).toBe(42)
  })

  test('rejects zero', () => {
    expect(parsePositiveIntId('0')).toBeUndefined()
  })

  test('rejects a negative number', () => {
    expect(parsePositiveIntId('-1')).toBeUndefined()
  })

  test('rejects non-numeric input', () => {
    expect(parsePositiveIntId('not-a-number')).toBeUndefined()
  })

  test('parses the leading digits of a mixed string, matching parseInt semantics', () => {
    expect(parsePositiveIntId('42abc')).toBe(42)
  })
})

describe('isValidComparator', () => {
  test('accepts every comparator the backend supports', () => {
    for (const op of ['>', '<', '>=', '<=']) {
      expect(isValidComparator(op)).toBe(true)
    }
  })

  test('rejects an unsupported operator', () => {
    expect(isValidComparator('=')).toBe(false)
    expect(isValidComparator('!=')).toBe(false)
    expect(isValidComparator('')).toBe(false)
  })
})

describe('isValidSeverity', () => {
  test('accepts "warning" and "critical"', () => {
    expect(isValidSeverity('warning')).toBe(true)
    expect(isValidSeverity('critical')).toBe(true)
  })

  test('rejects any other value', () => {
    expect(isValidSeverity('catastrophic')).toBe(false)
    expect(isValidSeverity('')).toBe(false)
  })
})
