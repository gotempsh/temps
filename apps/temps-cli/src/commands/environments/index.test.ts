import { test, expect, describe } from 'bun:test'
import {
  formatEnvVarValue,
  describeForceHttps,
  formatCpu,
  formatMemory,
  parseResourceUpdate,
  parseReplicaCount,
} from './index.js'
import type { EnvironmentVariableResponse } from '../../api/types.gen.js'

function makeVar(overrides: Partial<EnvironmentVariableResponse> = {}): EnvironmentVariableResponse {
  return {
    id: 1,
    key: 'DATABASE_URL',
    value: 'postgres://x',
    is_secret: false,
    include_in_preview: true,
    environments: [],
    ...overrides,
  } as EnvironmentVariableResponse
}

describe('formatEnvVarValue', () => {
  test('never prints a secret value, even if one somehow arrived on the wire', () => {
    // The API never returns secret plaintext, but the formatter itself must
    // not be the thing that would leak it if that contract ever broke.
    const v = makeVar({ is_secret: true, value: 'super-secret' })
    const out = formatEnvVarValue(v)
    expect(out).not.toContain('super-secret')
    expect(out).toContain('write-only')
  })

  test('shows the plain value for a non-secret variable', () => {
    expect(formatEnvVarValue(makeVar({ value: 'hello' }))).toBe('hello')
  })

  test('renders a missing value as empty rather than "undefined"', () => {
    expect(formatEnvVarValue(makeVar({ value: undefined }))).toBe('')
  })
})

describe('describeForceHttps', () => {
  test('true reads as an explicit always-redirect', () => {
    expect(describeForceHttps(true)).toContain('always redirect')
  })

  test('false reads as an explicit never-redirect', () => {
    expect(describeForceHttps(false)).toContain('never redirect')
  })

  test('null and undefined both read as "inherit", not "disabled"', () => {
    // A missing override must never look the same as an explicit --disable.
    expect(describeForceHttps(null)).toContain('inherit')
    expect(describeForceHttps(undefined)).toContain('inherit')
  })
})

describe('formatCpu', () => {
  test('renders millicores with the equivalent core count', () => {
    expect(formatCpu(500)).toBe('500m (0.5 CPU)')
    expect(formatCpu(1000)).toBe('1000m (1 CPU)')
  })

  test('renders an unset limit distinctly from 0', () => {
    expect(formatCpu(null)).toContain('not set')
    expect(formatCpu(undefined)).toContain('not set')
  })
})

describe('formatMemory', () => {
  test('renders sub-GB values in plain MB', () => {
    expect(formatMemory(512)).toBe('512MB')
  })

  test('adds a GB conversion once memory crosses 1024MB', () => {
    expect(formatMemory(2048)).toBe('2048MB (2.0GB)')
  })

  test('renders an unset limit distinctly from 0', () => {
    expect(formatMemory(null)).toContain('not set')
  })
})

describe('parseResourceUpdate', () => {
  test('rejects a non-numeric or non-positive CPU value', () => {
    expect(parseResourceUpdate({ cpu: 'abc' })).toEqual({
      error: 'CPU must be a positive number (millicores)',
    })
    expect(parseResourceUpdate({ cpu: '0' })).toEqual({
      error: 'CPU must be a positive number (millicores)',
    })
  })

  test('rejects a non-numeric or non-positive memory value', () => {
    expect(parseResourceUpdate({ memory: '-5' })).toEqual({
      error: 'Memory must be a positive number (MB)',
    })
  })

  test('defaults the request to the limit when no explicit request is given', () => {
    // Otherwise a container gets a limit with no guaranteed minimum, which
    // the scheduler treats as "no request" rather than "same as limit".
    const result = parseResourceUpdate({ cpu: '1000', memory: '512' })
    expect(result).toEqual({
      body: { cpu_limit: 1000, cpu_request: 1000, memory_limit: 512, memory_request: 512 },
    })
  })

  test('an explicit request overrides the limit-derived default', () => {
    const result = parseResourceUpdate({ cpu: '1000', cpuRequest: '250' })
    expect(result).toEqual({ body: { cpu_limit: 1000, cpu_request: 250 } })
  })

  test('rejects a non-positive explicit request even when the limit is valid', () => {
    expect(parseResourceUpdate({ cpu: '1000', cpuRequest: '0' })).toEqual({
      error: 'CPU request must be a positive number (millicores)',
    })
  })

  test('leaves fields untouched when nothing is set', () => {
    expect(parseResourceUpdate({})).toEqual({ body: {} })
  })
})

describe('parseReplicaCount', () => {
  test('accepts zero (scale to nothing)', () => {
    expect(parseReplicaCount('0')).toEqual({ replicas: 0 })
  })

  test('rejects a negative count', () => {
    expect(parseReplicaCount('-1')).toEqual({ error: 'Replicas must be a non-negative number' })
  })

  test('rejects non-numeric input', () => {
    expect(parseReplicaCount('many')).toEqual({ error: 'Replicas must be a non-negative number' })
  })

  test('warns, but still succeeds, above 10 replicas', () => {
    const result = parseReplicaCount('25')
    expect(result).toMatchObject({ replicas: 25 })
    expect('warning' in result && result.warning).toContain('25 replicas')
  })

  test('does not warn at or below 10 replicas', () => {
    expect(parseReplicaCount('10')).toEqual({ replicas: 10 })
  })
})
