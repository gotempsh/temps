// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { evaluateRules, normalizePath, type SampleRequest } from './evaluate'
import { newCondition, newGroup } from './expression'
import type {
  Condition,
  ConditionGroup,
  EdgeRule,
  Expression,
  RuleAction,
} from './types'

const sample = (overrides: Partial<SampleRequest> = {}): SampleRequest => ({
  method: 'GET',
  url: 'https://app.example.com/',
  ip: '198.51.100.20',
  country: 'US',
  continent: 'NA',
  asn: '64496',
  isHostingProvider: false,
  userAgent: 'Mozilla/5.0',
  headers: '',
  ja4: '',
  botClass: 'human',
  environmentId: null,
  challengeCleared: false,
  ...overrides,
})

// Explicitly case-sensitive unless a test says otherwise; the default for
// new path conditions is covered in expression.test.ts.
const cond = (c: Partial<Condition> & Pick<Condition, 'field'>): Condition => ({
  ...newCondition(c.field),
  caseInsensitive: false,
  ...c,
})

let nextId = 0
function rule(
  expression: Expression[] | ConditionGroup,
  action: RuleAction,
  overrides: Partial<EdgeRule> = {}
): EdgeRule {
  nextId += 1
  return {
    id: `r${nextId}`,
    name: `rule ${nextId}`,
    enabled: true,
    mode: 'enforce',
    environmentId: null,
    expression: Array.isArray(expression)
      ? newGroup('all', expression)
      : expression,
    action,
    revision: 1,
    createdAt: '2026-10-31T00:00:00Z',
    updatedAt: '2026-10-31T00:00:00Z',
    ...overrides,
  }
}

const block: RuleAction = { type: 'block', status: 403, body: '' }

describe('normalizePath', () => {
  test.each([
    ['/admin', '/admin', true],
    ['//admin', '/admin', false],
    ['/%61dmin', '/admin', false],
    ['/x/../admin', '/admin', false],
    ['/a/./b/', '/a/b/', false],
    ['/..', '/', false],
    ['/a%2Fb', '/a%2Fb', true],
    ['/a%2fb', '/a%2Fb', false],
    ['/a\\b', '/a\\b', true],
  ])('%s -> %s', (raw, path, canonical) => {
    expect(normalizePath(raw)).toEqual({ path, canonical })
  })
})

describe('evaluateRules', () => {
  test('forwards when nothing matches', () => {
    const result = evaluateRules(
      [
        rule(
          [cond({ field: 'path', op: 'starts_with', value: '/wp-' })],
          block
        ),
      ],
      sample()
    )
    expect(result.decision).toEqual({ kind: 'forward' })
    expect(result.trace[0].outcome).toBe('no_match')
  })

  test('first terminal match wins and later rules are not reached', () => {
    const first = rule(
      [cond({ field: 'path', op: 'glob', value: '/admin*' })],
      {
        type: 'allow',
      }
    )
    const second = rule(
      [cond({ field: 'path', op: 'glob', value: '/admin*' })],
      block
    )
    const result = evaluateRules(
      [first, second],
      sample({ url: '/admin/users' })
    )
    expect(result.decision).toEqual({ kind: 'allow', ruleId: first.id })
    expect(result.trace.map((t) => t.outcome)).toEqual([
      'decided',
      'not_reached',
    ])
  })

  test('office allowlist above a block creates an exception', () => {
    const office = rule(
      [
        cond({ field: 'path', op: 'glob', value: '/admin*' }),
        cond({ field: 'ip', op: 'in_cidr', value: '203.0.113.0/24' }),
      ],
      { type: 'allow' }
    )
    const blockAdmin = rule(
      [cond({ field: 'path', op: 'glob', value: '/admin*' })],
      block
    )
    expect(
      evaluateRules(
        [office, blockAdmin],
        sample({ url: '/admin', ip: '203.0.113.9' })
      ).decision.kind
    ).toBe('allow')
    expect(
      evaluateRules(
        [office, blockAdmin],
        sample({ url: '/admin', ip: '192.0.2.1' })
      ).decision.kind
    ).toBe('block')
  })

  test('log-only and log rules record a match and continue', () => {
    const observe = rule(
      [cond({ field: 'path', op: 'glob', value: '*' })],
      block,
      {
        mode: 'log_only',
      }
    )
    const log = rule([cond({ field: 'path', op: 'glob', value: '*' })], {
      type: 'log',
    })
    const result = evaluateRules([observe, log], sample())
    expect(result.decision.kind).toBe('forward')
    expect(result.trace.map((t) => t.outcome)).toEqual(['logged', 'logged'])
  })

  test('a cleared challenge continues to later rules', () => {
    const challenge = rule([], { type: 'challenge' })
    const blockAll = rule([], block)
    expect(evaluateRules([challenge, blockAll], sample()).decision.kind).toBe(
      'challenge'
    )
    const cleared = evaluateRules(
      [challenge, blockAll],
      sample({ challengeCleared: true })
    )
    expect(cleared.decision.kind).toBe('block')
    expect(cleared.trace[0].outcome).toBe('challenge_passed')
  })

  test('skips disabled rules and rules for other environments', () => {
    const disabled = rule([], block, { enabled: false })
    const staging = rule([], block, { environmentId: 2 })
    const result = evaluateRules(
      [disabled, staging],
      sample({ environmentId: 1 })
    )
    expect(result.decision.kind).toBe('forward')
    expect(result.trace.map((t) => t.outcome)).toEqual([
      'disabled',
      'other_environment',
    ])
  })

  test('normalized path blocks encoding tricks', () => {
    const blockAdmin = rule(
      [cond({ field: 'path', op: 'eq', value: '/admin' })],
      block
    )
    for (const url of ['/admin', '//admin', '/%61dmin', '/x/../admin']) {
      expect(evaluateRules([blockAdmin], sample({ url })).decision.kind).toBe(
        'block'
      )
    }
  })

  test('raw_path sees the encoding itself', () => {
    const traversal = rule(
      [cond({ field: 'raw_path', op: 'contains', value: '%2e%2e' })],
      block
    )
    expect(
      evaluateRules([traversal], sample({ url: '/a/%2e%2e/etc' })).decision.kind
    ).toBe('block')
  })

  test('allow rules on the path never match an ambiguous path, even negated', () => {
    // "allow everything except /admin" must not let //admin through.
    const allowNonAdmin = rule(
      newGroup('none', [cond({ field: 'path', op: 'glob', value: '/admin*' })]),
      { type: 'allow' }
    )
    const blockAll = rule([], block)
    const result = evaluateRules(
      [allowNonAdmin, blockAll],
      sample({ url: '/%61dmin' })
    )
    expect(result.trace[0].outcome).toBe('ambiguous_path')
    expect(result.decision.kind).toBe('block')
    expect(
      evaluateRules([allowNonAdmin, blockAll], sample({ url: '/home' }))
        .decision.kind
    ).toBe('allow')
  })

  test('path matching is case-sensitive unless the condition ignores case', () => {
    const exact = rule(
      [cond({ field: 'path', op: 'starts_with', value: '/admin' })],
      block
    )
    expect(
      evaluateRules([exact], sample({ url: '/ADMIN' })).decision.kind
    ).toBe('forward')
    const folded = rule(
      [
        cond({
          field: 'path',
          op: 'starts_with',
          value: '/admin',
          caseInsensitive: true,
        }),
      ],
      block
    )
    expect(
      evaluateRules([folded], sample({ url: '/ADMIN' })).decision.kind
    ).toBe('block')
  })

  describe('regular expressions', () => {
    const regex = (value: string) => cond({ field: 'path', op: 'regex', value })
    const outcomes = (rules: EdgeRule[], url: string) =>
      evaluateRules(rules, sample({ url })).trace.map((t) => t.outcome)

    test('a leading inline flag group runs as the proxy runs it', () => {
      const r = rule([regex('(?i)^/admin')], block)
      expect(
        evaluateRules([r], sample({ url: '/ADMIN/users' })).decision
      ).toEqual({ kind: 'block', ruleId: r.id, status: 403 })
      expect(outcomes([r], '/home')).toEqual(['no_match'])
    })

    test('a pattern the browser cannot run is never reported as a nonmatch', () => {
      // `\A` is Rust's start-of-text anchor; JavaScript has no such escape.
      for (const pattern of ['\\A/admin', '(?x) ^/admin', '(?P<area>admin)']) {
        const r = rule([regex(pattern)], block)
        const result = evaluateRules([r], sample({ url: '/admin' }))
        expect(result.decision).toEqual({ kind: 'unknown', ruleId: r.id })
        expect(result.trace.map((t) => t.outcome)).toEqual(['untestable'])
      }
    })

    test('Unicode-aware classes are only tested on ASCII text', () => {
      // Rust's \w matches "é"; JavaScript's does not, even with the u flag.
      const r = rule(
        [
          cond({
            field: 'query',
            key: 'name',
            op: 'regex',
            value: '(?i)^\\w+$',
          }),
        ],
        block
      )
      const unicode = evaluateRules([r], sample({ url: '/?name=caf%C3%A9' }))
      expect(unicode.decision).toEqual({ kind: 'unknown', ruleId: r.id })
      expect(unicode.trace.map((t) => t.outcome)).toEqual(['untestable'])
      expect(
        evaluateRules([r], sample({ url: '/?name=Cafe' })).decision.kind
      ).toBe('block')
      expect(outcomes([r], '/?name=caf-e')).toEqual(['no_match'])
      // An escaped backslash before "w" is a literal, not a class.
      const literal = rule([regex('^/caf\\\\w')], block)
      expect(outcomes([literal], '/café')).toEqual(['no_match'])
    })

    test('an untestable log-only rule lets later rules decide', () => {
      const observe = rule([regex('\\A/admin')], block, { mode: 'log_only' })
      const blockAll = rule([], block)
      const result = evaluateRules(
        [observe, blockAll],
        sample({ url: '/admin' })
      )
      expect(result.decision.kind).toBe('block')
      expect(result.trace.map((t) => t.outcome)).toEqual([
        'untestable',
        'decided',
      ])
    })

    test('groups only report unknown when the answer depends on it', () => {
      const untestable = regex('\\A/admin')
      const admin = cond({ field: 'path', op: 'starts_with', value: '/admin' })
      const home = cond({ field: 'path', op: 'starts_with', value: '/home' })
      const decide = (group: ConditionGroup) =>
        evaluateRules([rule(group, block)], sample({ url: '/admin' })).decision
          .kind
      expect(decide(newGroup('any', [untestable, admin]))).toBe('block')
      expect(decide(newGroup('all', [untestable, home]))).toBe('forward')
      expect(decide(newGroup('none', [untestable, admin]))).toBe('forward')
      expect(decide(newGroup('all', [untestable, admin]))).toBe('unknown')
      expect(decide(newGroup('none', [untestable, home]))).toBe('unknown')
    })
  })

  describe('headers', () => {
    const apiKey = (op: Condition['op'], value = 's3cret') =>
      cond({ field: 'header', key: 'X-Api-Key', op, value })

    test('a missing header never satisfies a value test', () => {
      expect(
        evaluateRules([rule([apiKey('eq')], block)], sample()).decision.kind
      ).toBe('forward')
    })

    test('"block unless the key is correct" blocks a request without the header', () => {
      const blockUnlessKey = rule(newGroup('none', [apiKey('eq')]), block)
      expect(evaluateRules([blockUnlessKey], sample()).decision.kind).toBe(
        'block'
      )
      expect(
        evaluateRules(
          [blockUnlessKey],
          sample({ headers: 'x-api-key: s3cret' })
        ).decision.kind
      ).toBe('forward')
    })

    test('names are case-insensitive and repeated headers match on any value', () => {
      const r = rule([apiKey('eq', 'two')], block)
      expect(
        evaluateRules(
          [r],
          sample({ headers: 'X-API-KEY: one\nx-api-key: two' })
        ).decision.kind
      ).toBe('block')
    })

    test('exists and not_exists test presence only', () => {
      expect(
        evaluateRules(
          [rule([apiKey('exists')], block)],
          sample({ headers: 'X-Api-Key:' })
        ).decision.kind
      ).toBe('block')
      expect(
        evaluateRules([rule([apiKey('not_exists')], block)], sample()).decision
          .kind
      ).toBe('block')
    })
  })

  test('query and cookie conditions', () => {
    const debug = rule(
      [cond({ field: 'query', key: 'debug', op: 'eq', value: '1' })],
      block
    )
    expect(
      evaluateRules([debug], sample({ url: '/?a=b&debug=1' })).decision.kind
    ).toBe('block')
    const session = rule(
      [cond({ field: 'cookie', key: 'session', op: 'exists', value: '' })],
      {
        type: 'allow',
      }
    )
    expect(
      evaluateRules(
        [session],
        sample({ headers: 'Cookie: theme=dark; session=abc' })
      ).decision.kind
    ).toBe('allow')
  })

  test('geo, network and bot fields', () => {
    const r = rule(
      newGroup('any', [
        cond({ field: 'country', op: 'in', value: 'es, PT' }),
        cond({ field: 'asn', op: 'eq', value: 'AS64500' }),
        cond({ field: 'is_hosting_provider', op: 'eq', value: 'true' }),
        cond({
          field: 'bot_class',
          op: 'in',
          value: 'ai_crawler, unknown_bot',
        }),
      ]),
      block
    )
    expect(evaluateRules([r], sample()).decision.kind).toBe('forward')
    expect(evaluateRules([r], sample({ country: 'ES' })).decision.kind).toBe(
      'block'
    )
    expect(evaluateRules([r], sample({ asn: '64500' })).decision.kind).toBe(
      'block'
    )
    expect(
      evaluateRules([r], sample({ isHostingProvider: true })).decision.kind
    ).toBe('block')
    expect(
      evaluateRules([r], sample({ botClass: 'ai_crawler' })).decision.kind
    ).toBe('block')
  })

  test('host comes from the URL without the port', () => {
    const r = rule(
      [cond({ field: 'host', op: 'eq', value: 'api.example.com' })],
      block
    )
    expect(
      evaluateRules([r], sample({ url: 'https://API.example.com:8443/x' }))
        .decision.kind
    ).toBe('block')
  })

  test('redirect fills placeholders from the normalized path and raw query', () => {
    const r = rule([cond({ field: 'path', op: 'glob', value: '/docs/*' })], {
      type: 'redirect',
      status: 308,
      target: 'https://docs.example.com{path}?{query}',
    })
    expect(
      evaluateRules([r], sample({ url: '/docs//intro?x=1' })).decision
    ).toEqual({
      kind: 'redirect',
      ruleId: r.id,
      status: 308,
      location: 'https://docs.example.com/docs/intro?x=1',
    })
  })

  test('an unparseable client IP never matches an IP condition', () => {
    const r = rule(
      [cond({ field: 'ip', op: 'in_cidr', value: '0.0.0.0/0, ::/0' })],
      block
    )
    expect(evaluateRules([r], sample({ ip: 'unknown' })).decision.kind).toBe(
      'forward'
    )
  })
})
