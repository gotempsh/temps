// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  describeReason,
  emptyDraft,
  fromDraft,
  toDraft,
  describeRule,
  evenSplit,
  parseAttributeLines,
  parseOperand,
  referencedVariants,
  validateRule,
  validateVariantKey,
  type FlagRule,
  type FlagVariant,
} from './flag-rules'

const variants: FlagVariant[] = [
  { key: 'control', value: false },
  { key: 'test', value: true },
]

describe('parseOperand', () => {
  test('a list is comma separated and keeps numbers and booleans typed', () => {
    expect(parseOperand('in', 'pro, 5, true, team')).toEqual({
      ok: true,
      value: ['pro', 5, true, 'team'],
    })
  })

  test('presence operators take no operand', () => {
    expect(parseOperand('exists', 'ignored')).toEqual({
      ok: true,
      value: undefined,
    })
  })

  test('comparisons need a number', () => {
    expect(parseOperand('gt', '10')).toEqual({ ok: true, value: 10 })
    expect(parseOperand('gt', 'ten').ok).toBe(false)
  })

  test('an invalid regular expression is caught before the request', () => {
    expect(parseOperand('matches', '([a-z]').ok).toBe(false)
    expect(parseOperand('matches', '.*@example\\.com')).toEqual({
      ok: true,
      value: '.*@example\\.com',
    })
  })

  test('an empty value is refused for operators that need one', () => {
    expect(parseOperand('eq', '  ').ok).toBe(false)
    expect(parseOperand('in', ' , ').ok).toBe(false)
  })
})

describe('validateRule', () => {
  const serving = (serve: FlagRule['serve']): FlagRule => ({
    conditions: [{ attribute: 'plan', operator: 'eq', value: 'pro' }],
    serve,
  })

  test('a split must use declared variants, once each, summing to 100', () => {
    expect(
      validateRule(
        serving({
          kind: 'split',
          split: [
            { variant: 'control', weight: 50 },
            { variant: 'test', weight: 50 },
          ],
        }),
        variants
      )
    ).toBeNull()
    expect(
      validateRule(
        serving({
          kind: 'split',
          split: [
            { variant: 'control', weight: 50 },
            { variant: 'test', weight: 40 },
          ],
        }),
        variants
      )
    ).toContain('add up to 90')
    expect(
      validateRule(
        serving({
          kind: 'split',
          split: [
            { variant: 'control', weight: 50 },
            { variant: 'control', weight: 50 },
          ],
        }),
        variants
      )
    ).toContain('twice')
    expect(
      validateRule(
        serving({ kind: 'split', split: [{ variant: 'test', weight: 100 }] }),
        variants
      )
    ).toContain('at least two')
  })

  test('serving an undeclared variant is refused', () => {
    expect(
      validateRule(serving({ kind: 'variant', variant: 'missing' }), variants)
    ).toContain('"missing"')
  })

  test('rollout must be a percentage', () => {
    expect(
      validateRule(
        { ...serving({ kind: 'value', value: true }), rollout_percentage: 101 },
        variants
      )
    ).toContain('between 0 and 100')
  })

  test('a condition needs an attribute', () => {
    expect(
      validateRule(
        {
          conditions: [{ attribute: ' ', operator: 'exists' }],
          serve: { kind: 'value', value: true },
        },
        variants
      )
    ).toContain('Condition 1')
  })
})

test('evenSplit always sums to 100', () => {
  for (const count of [1, 2, 3, 6, 7]) {
    const keys = Array.from({ length: count }, (_, i) => `v${i}`)
    const split = evenSplit(keys)
    expect(split.reduce((sum, arm) => sum + arm.weight, 0)).toBe(100)
  }
})

test('referencedVariants collects direct and split variants', () => {
  const used = referencedVariants([
    { conditions: [], serve: { kind: 'variant', variant: 'test' } },
    {
      conditions: [],
      serve: {
        kind: 'split',
        split: [
          { variant: 'control', weight: 50 },
          { variant: 'other', weight: 50 },
        ],
      },
    },
    { conditions: [], serve: { kind: 'value', value: true } },
  ])
  expect([...used].sort()).toEqual(['control', 'other', 'test'])
})

test('describeRule reads as a sentence', () => {
  expect(
    describeRule(
      {
        conditions: [
          { attribute: 'country', operator: 'in', value: ['ES', 'FR'] },
          { attribute: 'email', operator: 'exists' },
        ],
        rollout_percentage: 25,
        serve: { kind: 'value', value: true },
      },
      'bool'
    )
  ).toBe('country is one of ES, FR and email is set (25% of them) → On')
  expect(
    describeRule(
      { conditions: [], serve: { kind: 'variant', variant: 'test' } },
      'json'
    )
  ).toBe('Everyone → variant test')
})

test('validateVariantKey mirrors the server rule', () => {
  expect(validateVariantKey('control')).toBeNull()
  expect(validateVariantKey('Control')).not.toBeNull()
  expect(validateVariantKey('')).not.toBeNull()
  expect(validateVariantKey('a'.repeat(65))).not.toBeNull()
})

test('parseAttributeLines reads name=value lines and types scalars', () => {
  expect(parseAttributeLines('plan=pro\n\nseats=5\nbeta=true')).toEqual({
    ok: true,
    attributes: { plan: 'pro', seats: 5, beta: true },
  })
  expect(parseAttributeLines('plan').ok).toBe(false)
})

test('describeReason numbers rules from one', () => {
  expect(describeReason({ kind: 'RULE_MATCH', index: 0 })).toBe(
    'Rule 1 matched'
  )
  expect(describeReason({ kind: 'ERROR' })).toContain('could not be evaluated')
})

describe('drafts', () => {
  test('a rule survives a round trip through the editor', () => {
    const rule: FlagRule = {
      description: 'Pro plans in Spain',
      conditions: [
        { attribute: 'plan', operator: 'in', value: ['pro', 'team'] },
        { attribute: 'country', operator: 'eq', value: 'ES' },
        { attribute: 'email', operator: 'exists' },
      ],
      rollout_percentage: 25,
      serve: { kind: 'value', value: true },
    }
    const back = fromDraft(toDraft(rule, 'bool'), 'bool', variants, 1)
    expect(back).toEqual({ ok: true, rule })
  })

  test('a split round-trips', () => {
    const rule: FlagRule = {
      conditions: [],
      serve: {
        kind: 'split',
        split: [
          { variant: 'control', weight: 50 },
          { variant: 'test', weight: 50 },
        ],
      },
    }
    expect(fromDraft(toDraft(rule, 'bool'), 'bool', variants, 1)).toEqual({
      ok: true,
      rule,
    })
  })

  test('errors name the rule and condition', () => {
    const draft = emptyDraft('number', 1)
    const result = fromDraft(draft, 'number', variants, 3)
    expect(result).toEqual({
      ok: false,
      error: 'Rule 3, condition 1: choose an attribute.',
    })
  })

  test('the served value is parsed as the flag type', () => {
    const draft = {
      ...emptyDraft('number', 1),
      conditions: [],
      valueText: 'abc',
    }
    const result = fromDraft(draft, 'number', variants, 2)
    expect(result.ok).toBe(false)
  })
})
