import { describe, expect, test } from 'bun:test'
import {
  buildSetGovernanceBody,
  dollarsToMicrocents,
  formatAllowedModels,
  microcentsToDollars,
  parseAllowedModels,
} from './governance.js'

describe('dollarsToMicrocents', () => {
  test('converts a whole dollar amount', () => {
    expect(dollarsToMicrocents('50')).toBe(5_000_000_000)
  })

  test('converts a fractional dollar amount', () => {
    expect(dollarsToMicrocents('12.50')).toBe(1_250_000_000)
  })

  test('converts zero', () => {
    expect(dollarsToMicrocents('0')).toBe(0)
  })

  test('rejects a negative amount', () => {
    expect(() => dollarsToMicrocents('-1')).toThrow(/non-negative/)
  })

  test('rejects a non-numeric amount', () => {
    expect(() => dollarsToMicrocents('abc')).toThrow(/non-negative/)
  })
})

describe('microcentsToDollars', () => {
  test('converts back to a two-decimal dollar string', () => {
    expect(microcentsToDollars(5_000_000_000)).toBe('50.00')
  })

  test('round-trips with dollarsToMicrocents', () => {
    expect(microcentsToDollars(dollarsToMicrocents('12.50'))).toBe('12.50')
  })

  test('converts zero', () => {
    expect(microcentsToDollars(0)).toBe('0.00')
  })
})

describe('parseAllowedModels', () => {
  test('omitted flag leaves the allowlist unset (all models allowed)', () => {
    expect(parseAllowedModels(undefined)).toBeUndefined()
  })

  test('"none" blocks all models', () => {
    expect(parseAllowedModels('none')).toEqual([])
  })

  test('"none" is case-insensitive', () => {
    expect(parseAllowedModels('NONE')).toEqual([])
  })

  test('parses a comma-separated list', () => {
    expect(parseAllowedModels('gpt-4o,claude-3-5-sonnet')).toEqual([
      'gpt-4o',
      'claude-3-5-sonnet',
    ])
  })

  test('trims whitespace around entries', () => {
    expect(parseAllowedModels(' gpt-4o , claude-3-5-sonnet ')).toEqual([
      'gpt-4o',
      'claude-3-5-sonnet',
    ])
  })

  test('rejects an empty string', () => {
    expect(() => parseAllowedModels('')).toThrow(/cannot be empty/)
  })

  test('rejects a whitespace-only string', () => {
    expect(() => parseAllowedModels('   ')).toThrow(/cannot be empty/)
  })
})

describe('formatAllowedModels', () => {
  test('null renders as "all models"', () => {
    expect(formatAllowedModels(null)).toContain('all models')
  })

  test('empty array renders as blocked', () => {
    expect(formatAllowedModels([])).toContain('none')
  })

  test('a list renders as a comma-joined string', () => {
    expect(formatAllowedModels(['gpt-4o', 'claude-3-5-sonnet'])).toBe(
      'gpt-4o, claude-3-5-sonnet'
    )
  })
})

describe('buildSetGovernanceBody', () => {
  test('only includes fields the user actually passed', () => {
    expect(buildSetGovernanceBody({ rpm: '100' })).toEqual({
      max_requests_per_minute: 100,
    })
  })

  test('returns an empty body when nothing was passed', () => {
    expect(buildSetGovernanceBody({})).toEqual({})
  })

  test('includes all fields when all are passed', () => {
    expect(
      buildSetGovernanceBody({
        models: 'gpt-4o,claude-3-5-sonnet',
        rpm: '100',
        monthlyBudget: '50.00',
      })
    ).toEqual({
      allowed_models: ['gpt-4o', 'claude-3-5-sonnet'],
      max_requests_per_minute: 100,
      max_cost_per_month_microcents: 5_000_000_000,
    })
  })

  test('"none" produces an explicit empty allowlist, not an omitted field', () => {
    const body = buildSetGovernanceBody({ models: 'none' })
    expect(body.allowed_models).toEqual([])
  })

  test('rejects a negative rpm', () => {
    expect(() => buildSetGovernanceBody({ rpm: '-5' })).toThrow(/non-negative integer/)
  })

  test('rejects a non-numeric rpm', () => {
    expect(() => buildSetGovernanceBody({ rpm: 'abc' })).toThrow(/non-negative integer/)
  })
})
