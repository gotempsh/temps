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
  /** The rule uses a pattern the browser cannot run the way the proxy does,
   * so the tester cannot say whether it matches. */
  | 'untestable'

export interface RuleTrace {
  ruleId: string
  outcome: RuleOutcome
}

export type Decision =
  | { kind: 'forward' }
  /** An enforcing rule the browser cannot evaluate was reached, so what the
   * proxy decides depends on it. */
  | { kind: 'unknown'; ruleId: string }
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

/** A condition's result in the browser: `null` when it cannot be run here
 * the way the proxy runs it. */
export type MatchResult = boolean | null

interface BrowserRegex {
  re: RegExp
  /** The pattern uses `\w`, `\d`, `\s` or `\b` (or their negations). Rust
   * reads these as Unicode classes and JavaScript as ASCII ones, even with
   * the `u` flag, so the two agree only on ASCII text. */
  asciiOnly: boolean
}

// A Perl class or word boundary escape, not preceded by an escaped backslash.
const PERL_CLASS = /(?:^|[^\\])(?:\\\\)*\\[wWdDsSbB]/
const NON_ASCII = /[^\u0000-\u007f]/

const regexCache = new Map<string, BrowserRegex | null>()
/**
 * The proxy's pattern (Rust `regex` syntax) as a JavaScript RegExp, or `null`
 * when JavaScript cannot run it faithfully. A leading flag group such as
 * `(?i)` becomes the matching JavaScript flag. The `u` flag keeps JavaScript
 * from reading a Rust-only escape (`\A`, `\z`) as a literal letter; anything
 * it still cannot compile is reported as untestable rather than as a
 * nonmatch.
 */
function rustRegex(pattern: string, ci: boolean): BrowserRegex | null {
  const key = `${ci ? 'i' : 's'}:${pattern}`
  if (regexCache.has(key)) return regexCache.get(key) ?? null
  let source = pattern
  const flags = new Set(['u'])
  if (ci) flags.add('i')
  const leading = /^\(\?([a-zA-Z]+)\)/.exec(source)
  if (leading) {
    for (const flag of leading[1]) {
      if (flag === 'i' || flag === 'm' || flag === 's') flags.add(flag)
      // `x`, `U` and friends change the pattern in ways JavaScript has no
      // flag for.
      else return cache(regexCache, key, null)
    }
    source = source.slice(leading[0].length)
  }
  try {
    return cache(regexCache, key, {
      re: new RegExp(source, [...flags].join('')),
      asciiOnly: PERL_CLASS.test(source),
    })
  } catch {
    return cache(regexCache, key, null)
  }
}

function cache<T>(map: Map<string, T>, key: string, value: T): T {
  map.set(key, value)
  return value
}

function testString(c: Condition, candidate: string, ci: boolean): MatchResult {
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
    case 'regex': {
      const compiled = rustRegex(c.value.trim(), ci)
      if (!compiled) return null
      if (compiled.asciiOnly && NON_ASCII.test(candidate)) return null
      return compiled.re.test(candidate)
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

export function matchCondition(c: Condition, facts: RequestFacts): MatchResult {
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
  return anyOf(values.map((v) => testString(c, v, ci)))
}

/** True when one result is true; unknown when none is but one is unknown. */
function anyOf(results: MatchResult[]): MatchResult {
  if (results.some((r) => r === true)) return true
  return results.some((r) => r === null) ? null : false
}

/** False when one result is false; unknown when none is but one is unknown. */
function allOf(results: MatchResult[]): MatchResult {
  if (results.some((r) => r === false)) return false
  return results.some((r) => r === null) ? null : true
}

/** `null` when the outcome depends on a condition the browser cannot run. */
export function matchExpression(
  expr: Expression,
  facts: RequestFacts
): MatchResult {
  if (expr.kind === 'condition') return matchCondition(expr, facts)
  if (expr.children.length === 0) return true
  const results = expr.children.map((child) => matchExpression(child, facts))
  switch (expr.combinator) {
    case 'all':
      return allOf(results)
    case 'any':
      return anyOf(results)
    case 'none': {
      const any = anyOf(results)
      return any === null ? null : !any
    }
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
    const matched = matchExpression(rule.expression, facts)
    if (matched === null) {
      push('untestable')
      // A match that would only be logged, or a challenge the visitor has
      // already passed, lets the request on either way.
      const continues =
        rule.mode === 'log_only' ||
        rule.action.type === 'log' ||
        (rule.action.type === 'challenge' && sample.challengeCleared)
      if (!continues) {
        decided = true
        decision = { kind: 'unknown', ruleId: rule.id }
      }
      continue
    }
    if (!matched) {
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
