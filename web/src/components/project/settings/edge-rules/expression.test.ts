// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  autoName,
  describeExpression,
  emptyDraft,
  newCondition,
  newGroup,
  removeNode,
  ruleWarnings,
  updateNode,
  validateRule,
  withAutoName,
} from './expression'
import { LIMITS } from './fields'
import type { Condition, EdgeRuleDraft, RuleAction } from './types'

function draftWith(
  conditions: Array<Partial<Condition>>,
  action: RuleAction = { type: 'block', status: 403, body: '' }
): EdgeRuleDraft {
  const draft = emptyDraft()
  draft.name = 'Test rule'
  draft.action = action
  draft.expression = newGroup(
    'all',
    conditions.map((c) => ({
      ...newCondition(c.field ?? 'path'),
      caseInsensitive: false,
      ...c,
    }))
  )
  return draft
}

const messages = (draft: EdgeRuleDraft) =>
  validateRule(draft).map((i) => i.message)

describe('validateRule', () => {
  test('accepts a well-formed rule', () => {
    expect(validateRule(draftWith([{ op: 'glob', value: '/wp-*' }]))).toEqual(
      []
    )
  })

  test('requires a name and a value', () => {
    const draft = draftWith([{ op: 'eq', value: '' }])
    draft.name = '  '
    expect(validateRule(draft).map((i) => i.target)).toContain('name')
    expect(messages(draft)).toContain('Enter a value.')
  })

  test('requires a valid header name for header conditions', () => {
    expect(
      messages(draftWith([{ field: 'header', key: '', op: 'exists' }]))
    ).toContain('Enter the header name.')
    expect(
      messages(
        draftWith([{ field: 'header', key: 'bad header', op: 'exists' }])
      )
    ).toContain('"bad header" is not a valid header name.')
  })

  test('rejects invalid CIDRs, countries and AS numbers', () => {
    expect(
      messages(
        draftWith([{ field: 'ip', op: 'in_cidr', value: '10.0.0.0/8, nope' }])
      )[0]
    ).toContain('nope')
    expect(
      messages(
        draftWith([{ field: 'country', op: 'in', value: 'ES, Spain' }])
      )[0]
    ).toContain('Spain')
    expect(
      messages(draftWith([{ field: 'asn', op: 'eq', value: 'AS64496' }]))
    ).toEqual([])
  })

  test('rejects regex features the proxy regex engine does not support', () => {
    for (const value of ['^/(?=admin)', '(a)\\1', '(?<!x)y']) {
      expect(messages(draftWith([{ op: 'regex', value }]))).toContain(
        'Look-ahead, look-behind and back-references are not supported.'
      )
    }
  })

  test('limits glob wildcards', () => {
    expect(
      messages(draftWith([{ op: 'glob', value: '/*'.repeat(9) }]))
    ).toContain('Patterns may use at most 8 * wildcards.')
  })

  test('rejects empty nested groups but allows an empty root', () => {
    const draft = draftWith([])
    expect(validateRule(draft)).toEqual([])
    draft.expression.children.push(newGroup('any', []))
    expect(messages(draft)).toContain(
      'Add a condition to this group or remove it.'
    )
  })

  test('limits nesting depth to four levels', () => {
    const draft = draftWith([])
    let group = draft.expression
    for (let i = 0; i < 4; i++) {
      const child = newGroup('all')
      group.children.push(child)
      group = child
    }
    expect(messages(draft)).toContain(
      'Groups can be nested at most 4 levels deep.'
    )
  })

  test('limits block status to 4xx', () => {
    expect(
      messages(draftWith([], { type: 'block', status: 500, body: '' }))
    ).toContain('Block status must be between 400 and 499.')
  })

  describe('redirect targets cannot become open redirects', () => {
    const redirect = (target: string) =>
      messages(draftWith([], { type: 'redirect', target, status: 302 }))

    test('accepts fixed-host URLs and paths with placeholders', () => {
      expect(redirect('https://docs.example.com{path}')).toEqual([])
      expect(redirect('https://docs.example.com/new?{query}')).toEqual([])
      expect(redirect('/new{path}')).toEqual([])
    })

    test('rejects a placeholder that can extend the host', () => {
      expect(redirect('https://example.com{query}').length).toBeGreaterThan(0)
      expect(redirect('https://example.com.{path}').length).toBe(0)
      expect(redirect('https://example{query}.com').length).toBeGreaterThan(0)
    })

    test('rejects protocol-relative path targets', () => {
      expect(redirect('//evil.example')).not.toEqual([])
      expect(redirect('/\\evil.example')).not.toEqual([])
      expect(redirect('/{path}')).not.toEqual([])
    })

    test('rejects unknown placeholders', () => {
      expect(redirect('/x/{host}')).toContain(
        'Unknown placeholder {host}. Use {path} or {query}.'
      )
    })
  })
})

describe('ruleWarnings', () => {
  test('warns about spoofable forwarding headers', () => {
    const warnings = ruleWarnings(
      draftWith([
        {
          field: 'header',
          key: 'X-Forwarded-For',
          op: 'contains',
          value: '10.',
        },
      ])
    )
    expect(warnings.some((w) => w.includes('can be forged'))).toBe(true)
  })

  test('warns that a rule without conditions matches everything', () => {
    expect(ruleWarnings(draftWith([]))[0]).toContain('matches every request')
  })

  test('warns when an allow rule ignores path case', () => {
    const warnings = ruleWarnings(
      draftWith(
        [{ op: 'starts_with', value: '/admin', caseInsensitive: true }],
        {
          type: 'allow',
        }
      )
    )
    expect(warnings.some((w) => w.includes('widens'))).toBe(true)
  })
})

describe('tree edits and descriptions', () => {
  test('updates and removes nodes immutably', () => {
    const condition = newCondition('path')
    const root = newGroup('all', [condition])
    const updated = updateNode(
      root,
      condition.id,
      (n) => ({ ...n, value: '/x' }) as Condition
    )
    expect((updated.children[0] as Condition).value).toBe('/x')
    expect((root.children[0] as Condition).value).toBe('')
    expect(removeNode(updated, condition.id).children).toEqual([])
  })

  test('describes nested groups in plain English', () => {
    const draft = draftWith([{ op: 'starts_with', value: '/wp-' }])
    draft.expression.children.push(
      newGroup('none', [
        { ...newCondition('country'), op: 'in', value: 'ES, PT' },
      ])
    )
    expect(describeExpression(draft.expression)).toBe(
      'path starts with /wp- and none of (country is any of ES, PT)'
    )
  })
})

describe('automatic names', () => {
  test('names a one-condition rule after its action and condition', () => {
    const draft = draftWith([{ op: 'starts_with', value: '/admin' }])
    expect(autoName(draft)).toBe('Block when path starts with /admin')
  })

  test('counts the remaining conditions instead of listing them', () => {
    const draft = draftWith([
      { op: 'starts_with', value: '/admin' },
      { field: 'country', op: 'in', value: 'ES, PT' },
      { field: 'method', op: 'eq', value: 'POST' },
    ])
    expect(autoName(draft)).toBe(
      'Block when path starts with /admin and 2 more conditions'
    )
  })

  test('keeps the inversion of a none-of rule in the name', () => {
    const draft = draftWith([{ field: 'country', op: 'in', value: 'ES' }])
    draft.expression = { ...draft.expression, combinator: 'none' }
    expect(autoName(draft)).toBe('Block when none of (country is any of ES)')
  })

  test('truncates to the name limit', () => {
    const draft = draftWith([
      { op: 'starts_with', value: `/${'a'.repeat(200)}` },
    ])
    const name = autoName(draft)
    expect(name.length).toBe(LIMITS.nameLength)
    expect(name.endsWith('…')).toBe(true)
  })

  test('fills a blank name and leaves a given name alone', () => {
    const draft = draftWith([{ op: 'starts_with', value: '/x' }])
    expect(withAutoName(draft).name).toBe('Test rule')
    const blank = { ...draft, name: '   ' }
    expect(withAutoName(blank).name).toBe('Block when path starts with /x')
    expect(validateRule(withAutoName(blank))).toEqual([])
  })

  test('paths default to prefix matching that ignores case', () => {
    expect(newCondition('path').op).toBe('starts_with')
    expect(newCondition('path').caseInsensitive).toBe(true)
    expect(newCondition('header').caseInsensitive).toBe(false)
    expect(newCondition('country').caseInsensitive).toBeUndefined()
  })
})
