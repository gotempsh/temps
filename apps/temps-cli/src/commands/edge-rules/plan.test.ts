// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { canonical, describeDecision, parseHeaders, parseRulesFile, planApply, planIsEmpty, toSpec, type SavedRule } from './plan.js'

const expression = {
  combinator: 'all',
  children: [{ kind: 'condition', field: 'path', op: 'starts_with', values: ['/admin'], case_insensitive: true }],
}

function saved(id: number, name: string, extra: Partial<SavedRule> = {}): SavedRule {
  return {
    id,
    revision: 1,
    name,
    enabled: true,
    mode: 'enforce',
    environment_id: null,
    expression,
    action: { type: 'block', status: 403 },
    ...extra,
  }
}

describe('parseRulesFile', () => {
  test('accepts the export format and a bare array', () => {
    const rule = { name: 'Block admin', expression, action: { type: 'block', status: 403 } }
    expect(parseRulesFile(JSON.stringify({ rules: [rule] }), 'f').rules).toHaveLength(1)
    expect(parseRulesFile(JSON.stringify([rule]), 'f').rules[0]?.name).toBe('Block admin')
  })

  test('names the file and rule for each problem', () => {
    expect(() => parseRulesFile('{', 'rules.json')).toThrow('rules.json is not valid JSON')
    expect(() => parseRulesFile('{"rules": [{"expression": {}, "action": {}}]}', 'rules.json')).toThrow(
      'rules.json rule 1 needs a "name"',
    )
    const twice = [
      { name: 'A', expression, action: { type: 'log' } },
      { name: 'a', expression, action: { type: 'log' } },
    ]
    expect(() => parseRulesFile(JSON.stringify(twice), 'f')).toThrow('used twice')
    expect(() =>
      parseRulesFile(JSON.stringify([{ name: 'A', expression, action: { type: 'log' }, mode: 'sometimes' }]), 'f'),
    ).toThrow('mode must be')
    expect(() =>
      parseRulesFile(JSON.stringify([{ name: 'A', expression, action: { type: 'log' }, environment_id: -1 }]), 'f'),
    ).toThrow('environment_id')
  })
})

describe('planApply', () => {
  test('matches by name, updates changed rules in place and deletes the rest', () => {
    const current = [saved(1, 'Block admin'), saved(2, 'Old rule'), saved(3, 'Same')]
    const plan = planApply(current, [
      { name: 'Same', enabled: true, mode: 'enforce', environment_id: null, expression, action: { type: 'block', status: 403 } },
      { name: 'block admin', mode: 'log_only', expression, action: { type: 'block', status: 403 } },
      { name: 'New rule', expression, action: { type: 'log' } },
    ])
    expect(plan.create.map((r) => r.name)).toEqual(['New rule'])
    expect(plan.update.map((u) => u.id)).toEqual([1])
    expect(plan.remove).toEqual([{ id: 2, name: 'Old rule' }])
    expect(plan.unchanged).toEqual(['Same'])
    expect(plan.order).toEqual(['Same', 'block admin', 'New rule'])
  })

  test('editor-only ids in the saved expression do not count as a change', () => {
    const withIds = {
      id: 'g1',
      combinator: 'all',
      children: [{ ...expression.children[0], id: 'c1' }],
    }
    const plan = planApply([saved(1, 'Block admin', { expression: withIds })], [toSpec(saved(1, 'Block admin'))])
    expect(plan.update).toHaveLength(0)
    expect(planIsEmpty(plan, ['Block admin'])).toBe(true)
  })

  test('a reorder alone is a change', () => {
    const a = saved(1, 'A')
    const b = saved(2, 'B')
    const plan = planApply([a, b], [toSpec(b), toSpec(a)])
    expect(plan.update).toHaveLength(0)
    expect(planIsEmpty(plan, ['A', 'B'])).toBe(false)
  })

  test('export then apply is a no-op', () => {
    const current = [saved(1, 'A', { mode: 'log_only', environment_id: 4 }), saved(2, 'B', { enabled: false })]
    const plan = planApply(current, current.map(toSpec))
    expect(planIsEmpty(plan, ['A', 'B'])).toBe(true)
  })
})

describe('helpers', () => {
  test('canonical drops ids and sorts keys', () => {
    expect(JSON.stringify(canonical({ b: 1, id: 'x', a: [{ id: 'y', z: 2 }] }))).toBe('{"a":[{"z":2}],"b":1}')
  })

  test('parseHeaders splits on the first colon', () => {
    expect(parseHeaders(['Referer: https://a.test/x'])).toEqual([{ name: 'Referer', value: 'https://a.test/x' }])
    expect(() => parseHeaders(['nocolon'])).toThrow('Name: value')
  })

  test('describeDecision names the rule and outcome', () => {
    expect(describeDecision({ kind: 'forward' })).toContain('no rule decided')
    expect(describeDecision({ kind: 'block', status: 403, rule_id: 7 })).toBe('blocked with 403 by rule 7')
    expect(describeDecision({ kind: 'redirect', status: 308, location: '/new', rule_id: 2 })).toContain('/new')
  })
})
