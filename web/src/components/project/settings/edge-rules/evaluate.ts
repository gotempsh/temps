// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// In-browser evaluator for the "Test a request" panel. It implements the
// semantics in ADR-071 §2 and §4 so the UI can be validated before the proxy
// enforces anything; the proxy's Rust evaluator must agree with it.

import { fieldMeta, LIST_OPERATORS } from './fields'
import { referencesField, splitList } from './expression'
import { cidrContains, parseAsn, parseCidr, parseIp } from './ip'
import type { Condition, EdgeRule, Expression, RedirectStatus } from './types'

export interface SampleRequest {
  method: string
  /** Absolute URL or origin-form path, as the client sent it. */
  url: string
  ip: string
  country: string
  continent: string
  asn: string
  isHostingProvider: boolean
  userAgent: string
  /** One `Name: value` per line. */
  headers: string
  ja4: string
  botClass: string
  environmentId: number | null
  /** The visitor already holds challenge clearance. */
  challengeCleared: boolean
}

export interface RequestFacts {
  method: string
  host: string
  rawPath: string
  path: string
  canonicalPath: boolean
  query: Array<[string, string]>
  rawQuery: string
  headers: Array<[string, string]>
  cookies: Array<[string, string]>
  sample: SampleRequest
}

export type RuleOutcome =
  | 'disabled'
  | 'other_environment'
  | 'not_reached'
  | 'no_match'
  | 'ambiguous_path'
  | 'logged'
  | 'challenge_passed'
  | 'decided'

export interface RuleTrace {
  ruleId: string
  outcome: RuleOutcome
}

export type Decision =
  | { kind: 'forward' }
  | { kind: 'block'; ruleId: string; status: number }
  | { kind: 'allow'; ruleId: string }
  | { kind: 'challenge'; ruleId: string }
  | {
      kind: 'redirect'
      ruleId: string
      status: RedirectStatus
      location: string
    }

export interface EvaluationResult {
  decision: Decision
  trace: RuleTrace[]
  facts: RequestFacts
}

// ---------------------------------------------------------------------------
// Request parsing
// ---------------------------------------------------------------------------

/** RFC 3986 normalization used for the `path` field: decode unreserved
 * percent-escapes, collapse repeated slashes and resolve dot segments. A
 * backslash stays a literal character. */
export function normalizePath(raw: string): {
  path: string
  canonical: boolean
} {
  let p = raw.startsWith('/') ? raw : `/${raw}`
  p = p.replace(/%([0-9a-fA-F]{2})/g, (_, hex: string) => {
    const ch = String.fromCharCode(parseInt(hex, 16))
    return /[A-Za-z0-9\-._~]/.test(ch) ? ch : `%${hex.toUpperCase()}`
  })
  p = p.replace(/\/{2,}/g, '/')
  const input = p.split('/').slice(1)
  const out: string[] = []
  input.forEach((segment, i) => {
    const last = i === input.length - 1
    if (segment === '.') {
      if (last) out.push('')
      return
    }
    if (segment === '..') {
      out.pop()
      if (last) out.push('')
      return
    }
    out.push(segment)
  })
  const path = `/${out.join('/')}`
  return { path, canonical: path === raw }
}

function safeDecode(s: string): string {
  try {
    return decodeURIComponent(s.replace(/\+/g, ' '))
  } catch {
    return s
  }
}

export function requestFacts(sample: SampleRequest): RequestFacts {
  const url = sample.url.trim()
  const match = /^[a-z][a-z0-9+.-]*:\/\/([^/?#]*)([^?#]*)(?:\?([^#]*))?/i.exec(
    url
  )
  let host = ''
  let rawPath: string
  let rawQuery: string
  if (match) {
    host = match[1]
    rawPath = match[2] || '/'
    rawQuery = match[3] ?? ''
  } else {
    const [beforeHash] = url.split('#')
    const q = beforeHash.indexOf('?')
    rawPath = (q === -1 ? beforeHash : beforeHash.slice(0, q)) || '/'
    rawQuery = q === -1 ? '' : beforeHash.slice(q + 1)
  }
  host = host.replace(/^.*@/, '').toLowerCase()
  host = host.startsWith('[')
    ? host.replace(/\]:\d+$/, ']')
    : host.replace(/:\d+$/, '')

  const headers: Array<[string, string]> = []
  for (const line of sample.headers.split('\n')) {
    const colon = line.indexOf(':')
    if (colon <= 0) continue
    headers.push([
      line.slice(0, colon).trim().toLowerCase(),
      line.slice(colon + 1).trim(),
    ])
  }
  if (sample.userAgent.trim() && !headers.some(([n]) => n === 'user-agent')) {
    headers.push(['user-agent', sample.userAgent.trim()])
  }
  if (host && !headers.some(([n]) => n === 'host')) headers.push(['host', host])

  const cookies: Array<[string, string]> = []
  for (const [name, value] of headers) {
    if (name !== 'cookie') continue
    for (const pair of value.split(';')) {
      const eq = pair.indexOf('=')
      if (eq <= 0) continue
      cookies.push([pair.slice(0, eq).trim(), pair.slice(eq + 1).trim()])
    }
  }

  const query: Array<[string, string]> = rawQuery
    ? rawQuery
        .split('&')
        .filter(Boolean)
        .map((pair) => {
          const eq = pair.indexOf('=')
          return eq === -1
            ? [safeDecode(pair), '']
            : [safeDecode(pair.slice(0, eq)), safeDecode(pair.slice(eq + 1))]
        })
    : []

  const { path, canonical } = normalizePath(rawPath)
  return {
    method: sample.method.toUpperCase(),
    host,
    rawPath,
    path,
    canonicalPath: canonical,
    query,
    rawQuery,
    headers,
    cookies,
    sample,
  }
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

const globCache = new Map<string, RegExp>()
function globRegex(pattern: string, ci: boolean): RegExp {
  const key = `${ci ? 'i' : 's'}:${pattern}`
  let re = globCache.get(key)
  if (!re) {
    const source = pattern
      .split('*')
      .map((part) => part.replace(/[.+?^${}()|[\]\\]/g, '\\$&'))
      .join('.*')
    re = new RegExp(`^${source}$`, ci ? 'is' : 's')
    globCache.set(key, re)
  }
  return re
}

function testString(c: Condition, candidate: string, ci: boolean): boolean {
  const fold = (s: string) => (ci ? s.toLowerCase() : s)
  const v = fold(candidate)
  const value = fold(c.value.trim())
  switch (c.op) {
    case 'eq':
      return v === value
    case 'in':
      return splitList(c.value).some((item) => fold(item) === v)
    case 'starts_with':
      return v.startsWith(value)
    case 'ends_with':
      return v.endsWith(value)
    case 'contains':
      return v.includes(value)
    case 'glob':
      return globRegex(c.value.trim(), ci).test(candidate)
    case 'regex':
      try {
        return new RegExp(c.value.trim(), ci ? 'i' : '').test(candidate)
      } catch {
        return false
      }
    default:
      return false
  }
}

/** Every value the request carries for the condition's field. An empty list
 * means the value is missing. */
function candidates(c: Condition, facts: RequestFacts): string[] {
  const key = (c.key ?? '').trim()
  const s = facts.sample
  switch (c.field) {
    case 'path':
      return [facts.path]
    case 'raw_path':
      return [facts.rawPath]
    case 'host':
      return facts.host ? [facts.host] : []
    case 'method':
      return [facts.method]
    case 'header':
      return facts.headers
        .filter(([n]) => n === key.toLowerCase())
        .map(([, v]) => v)
    case 'query':
      return facts.query.filter(([n]) => n === key).map(([, v]) => v)
    case 'cookie':
      return facts.cookies.filter(([n]) => n === key).map(([, v]) => v)
    case 'user_agent':
      return facts.headers.filter(([n]) => n === 'user-agent').map(([, v]) => v)
    case 'country':
      return s.country.trim() ? [s.country.trim().toUpperCase()] : []
    case 'continent':
      return s.continent ? [s.continent] : []
    case 'asn':
      return s.asn.trim() ? [s.asn.trim().replace(/^AS/i, '')] : []
    case 'is_hosting_provider':
      return [String(s.isHostingProvider)]
    case 'ja4':
      return s.ja4.trim() ? [s.ja4.trim()] : []
    case 'bot_class':
      return s.botClass ? [s.botClass] : []
    case 'ip':
      return s.ip.trim() ? [s.ip.trim()] : []
  }
}

export function matchCondition(c: Condition, facts: RequestFacts): boolean {
  const values = candidates(c, facts)
  if (c.op === 'exists') return values.length > 0
  if (c.op === 'not_exists') return values.length === 0
  // A missing value never satisfies a value test (ADR-071 §2, header
  // matching). Negation is expressed with a "none of" group instead.
  if (values.length === 0) return false

  if (c.op === 'in_cidr') {
    const ip = parseIp(values[0])
    if (!ip) return false
    return splitList(c.value).some((entry) => {
      const cidr = parseCidr(entry)
      return cidr !== null && cidrContains(cidr, ip)
    })
  }

  const meta = fieldMeta(c.field)
  // Hosts, methods and the enumerated GeoIP fields compare without case;
  // free-text fields honour the per-condition flag.
  const ci =
    c.field === 'host' ||
    c.field === 'method' ||
    c.field === 'country' ||
    meta.valueKind === 'options' ||
    !!c.caseInsensitive
  if (c.field === 'asn') {
    // Compared as numbers, like the engine: "AS064496" matches 64496.
    const asn = parseAsn(values[0] ?? '')
    if (asn === null) return false
    const list = LIST_OPERATORS.has(c.op) ? splitList(c.value) : [c.value]
    return list.some((v) => parseAsn(v) === asn)
  }
  return values.some((v) => testString(c, v, ci))
}

export function matchExpression(
  expr: Expression,
  facts: RequestFacts
): boolean {
  if (expr.kind === 'condition') return matchCondition(expr, facts)
  if (expr.children.length === 0) return true
  switch (expr.combinator) {
    case 'all':
      return expr.children.every((child) => matchExpression(child, facts))
    case 'any':
      return expr.children.some((child) => matchExpression(child, facts))
    case 'none':
      return !expr.children.some((child) => matchExpression(child, facts))
  }
}

// ---------------------------------------------------------------------------
// Rule list evaluation (ADR-071 §4)
// ---------------------------------------------------------------------------

/** `{path}` is the normalized path including its leading slash, so
 * `https://docs.example.com{path}` keeps the visitor's path. `{query}` is the
 * raw query string without the `?`. */
function redirectLocation(target: string, facts: RequestFacts): string {
  return target
    .trim()
    .split('{path}')
    .join(facts.path)
    .split('{query}')
    .join(facts.rawQuery)
}

export function evaluateRules(
  rules: EdgeRule[],
  sample: SampleRequest
): EvaluationResult {
  const facts = requestFacts(sample)
  const trace: RuleTrace[] = []
  let decision: Decision = { kind: 'forward' }
  let decided = false

  for (const rule of rules) {
    const push = (outcome: RuleOutcome) =>
      trace.push({ ruleId: rule.id, outcome })
    if (!rule.enabled) {
      push('disabled')
      continue
    }
    if (
      rule.environmentId !== null &&
      sample.environmentId !== null &&
      rule.environmentId !== sample.environmentId
    ) {
      push('other_environment')
      continue
    }
    if (decided) {
      push('not_reached')
      continue
    }
    // An Allow rule that looks at the path must not match an ambiguous path:
    // the upstream may read it differently, so ambiguity can only cause an
    // extra block, never an extra allow. This applies to the whole rule, not
    // the single condition, so a "none of" group cannot invert it.
    if (
      rule.action.type === 'allow' &&
      !facts.canonicalPath &&
      referencesField(rule.expression, 'path')
    ) {
      push('ambiguous_path')
      continue
    }
    if (!matchExpression(rule.expression, facts)) {
      push('no_match')
      continue
    }
    if (rule.mode === 'log_only' || rule.action.type === 'log') {
      push('logged')
      continue
    }
    if (rule.action.type === 'challenge' && sample.challengeCleared) {
      push('challenge_passed')
      continue
    }
    push('decided')
    decided = true
    const action = rule.action
    switch (action.type) {
      case 'block':
        decision = { kind: 'block', ruleId: rule.id, status: action.status }
        break
      case 'allow':
        decision = { kind: 'allow', ruleId: rule.id }
        break
      case 'challenge':
        decision = { kind: 'challenge', ruleId: rule.id }
        break
      case 'redirect':
        decision = {
          kind: 'redirect',
          ruleId: rule.id,
          status: action.status,
          location: redirectLocation(action.target, facts),
        }
        break
    }
  }
  return { decision, trace, facts }
}
