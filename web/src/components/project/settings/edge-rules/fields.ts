// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ActionType, FieldId, Operator } from './types'

/** How a field's value is entered. `options` fields offer a fixed set. */
export type ValueKind = 'text' | 'cidr' | 'number' | 'options' | 'boolean'

export interface FieldOption {
  value: string
  label: string
}

export interface FieldMeta {
  id: FieldId
  label: string
  /** Shown under the field selector. */
  hint: string
  operators: Operator[]
  valueKind: ValueKind
  options?: FieldOption[]
  /** Header, query and cookie conditions name the item they test. */
  keyed?: 'header' | 'query parameter' | 'cookie'
  /** Supports the per-condition `case_insensitive` flag. */
  caseFlag?: boolean
  /** Values come from the GeoIP database. */
  geo?: boolean
  placeholder?: string
}

export const OPERATOR_LABELS: Record<Operator, string> = {
  eq: 'is',
  in: 'is any of',
  in_cidr: 'is in',
  starts_with: 'starts with',
  ends_with: 'ends with',
  contains: 'contains',
  glob: 'matches pattern',
  regex: 'matches regex',
  exists: 'exists',
  not_exists: 'does not exist',
}

/** Operators that take a list of values. */
export const LIST_OPERATORS: ReadonlySet<Operator> = new Set(['in', 'in_cidr'])

/** Operators that take no value. */
export const PRESENCE_OPERATORS: ReadonlySet<Operator> = new Set([
  'exists',
  'not_exists',
])

const METHODS = ['GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE', 'OPTIONS']

export const CONTINENTS: FieldOption[] = [
  { value: 'AF', label: 'Africa' },
  { value: 'AN', label: 'Antarctica' },
  { value: 'AS', label: 'Asia' },
  { value: 'EU', label: 'Europe' },
  { value: 'NA', label: 'North America' },
  { value: 'OC', label: 'Oceania' },
  { value: 'SA', label: 'South America' },
]

export const BOT_CLASSES: FieldOption[] = [
  { value: 'human', label: 'Human' },
  { value: 'search_engine', label: 'Search engine' },
  { value: 'ai_crawler', label: 'AI crawler' },
  { value: 'ai_agent', label: 'AI agent' },
  { value: 'unknown_bot', label: 'Unknown bot' },
]

const STRING_OPS: Operator[] = [
  'eq',
  'in',
  'starts_with',
  'ends_with',
  'contains',
  'glob',
  'regex',
]
const KEYED_OPS: Operator[] = [
  'exists',
  'not_exists',
  'eq',
  'in',
  'contains',
  'glob',
  'regex',
]

export const FIELDS: FieldMeta[] = [
  {
    id: 'path',
    label: 'Path',
    hint: 'Normalized, without the query string. //admin and /%61dmin both read as /admin.',
    // Prefix matching is what most path rules want, so it is the default.
    operators: [
      'starts_with',
      ...STRING_OPS.filter((op) => op !== 'starts_with'),
    ],
    valueKind: 'text',
    caseFlag: true,
    placeholder: '/admin*',
  },
  {
    id: 'raw_path',
    label: 'Raw path',
    hint: 'Exactly as received, before normalization. Use it to block encoding tricks such as %2e%2e.',
    operators: ['contains', 'glob', 'regex'],
    valueKind: 'text',
    caseFlag: true,
    placeholder: '%2e%2e',
  },
  {
    id: 'host',
    label: 'Host',
    hint: 'Lowercase hostname without the port.',
    operators: ['eq', 'in', 'ends_with', 'glob'],
    valueKind: 'text',
    placeholder: 'api.example.com',
  },
  {
    id: 'method',
    label: 'Method',
    hint: 'HTTP request method.',
    operators: ['eq', 'in'],
    valueKind: 'options',
    options: METHODS.map((m) => ({ value: m, label: m })),
  },
  {
    id: 'ip',
    label: 'Client IP',
    hint: 'IPv4 or IPv6 addresses and CIDR ranges, resolved through trusted proxies.',
    operators: ['in_cidr'],
    valueKind: 'cidr',
    placeholder: '203.0.113.0/24, 2001:db8::/32',
  },
  {
    id: 'country',
    label: 'Country',
    hint: 'Two-letter ISO country code from the GeoIP database.',
    operators: ['eq', 'in'],
    valueKind: 'text',
    geo: true,
    placeholder: 'ES, PT',
  },
  {
    id: 'continent',
    label: 'Continent',
    hint: 'From the GeoIP database.',
    operators: ['eq', 'in'],
    valueKind: 'options',
    options: CONTINENTS,
    geo: true,
  },
  {
    id: 'asn',
    label: 'Network (ASN)',
    hint: 'Autonomous system number from the GeoIP database.',
    operators: ['eq', 'in'],
    valueKind: 'number',
    geo: true,
    placeholder: '64496',
  },
  {
    id: 'is_hosting_provider',
    label: 'Hosting provider network',
    hint: 'True for cloud and data-centre networks, where most automated traffic originates.',
    operators: ['eq'],
    valueKind: 'boolean',
    geo: true,
  },
  {
    id: 'header',
    label: 'Header',
    hint: 'Names are case-insensitive. A missing header never matches a value test.',
    operators: KEYED_OPS,
    valueKind: 'text',
    keyed: 'header',
    caseFlag: true,
    placeholder: 'value',
  },
  {
    id: 'query',
    label: 'Query parameter',
    hint: 'A missing parameter never matches a value test.',
    operators: ['exists', 'not_exists', 'eq', 'contains', 'glob', 'regex'],
    valueKind: 'text',
    keyed: 'query parameter',
    caseFlag: true,
    placeholder: 'value',
  },
  {
    id: 'cookie',
    label: 'Cookie',
    hint: 'Cookie values are never written to logs.',
    operators: ['exists', 'not_exists', 'eq'],
    valueKind: 'text',
    keyed: 'cookie',
    placeholder: 'value',
  },
  {
    id: 'user_agent',
    label: 'User agent',
    hint: 'Client-supplied and easy to fake. Combine it with other conditions.',
    operators: ['contains', 'glob', 'regex'],
    valueKind: 'text',
    caseFlag: true,
    placeholder: 'curl',
  },
  {
    id: 'ja4',
    label: 'TLS fingerprint (JA4)',
    hint: 'Identifies the client TLS stack, not an individual visitor. HTTPS only.',
    operators: ['eq', 'in'],
    valueKind: 'text',
    placeholder: 't13d1516h2_8daaf6152771_02713d6af862',
  },
  {
    id: 'bot_class',
    label: 'Bot class',
    hint: 'From Temps crawler and AI-agent detection.',
    operators: ['eq', 'in'],
    valueKind: 'options',
    options: BOT_CLASSES,
  },
]

const FIELD_BY_ID = new Map(FIELDS.map((f) => [f.id, f]))

export function fieldMeta(id: FieldId): FieldMeta {
  const meta = FIELD_BY_ID.get(id)
  if (!meta) throw new Error(`Unknown edge rule field "${id}"`)
  return meta
}

/** Headers a client can set to anything unless a trusted proxy rewrites them. */
export const SPOOFABLE_HEADERS: ReadonlySet<string> = new Set([
  'x-forwarded-for',
  'x-real-ip',
  'forwarded',
  'cf-connecting-ip',
  'true-client-ip',
  'x-client-ip',
])

export interface ActionMeta {
  type: ActionType
  label: string
  description: string
  terminal: boolean
}

export const ACTIONS: ActionMeta[] = [
  {
    type: 'block',
    label: 'Block',
    description: 'Return an error response. Later rules are not checked.',
    terminal: true,
  },
  {
    type: 'allow',
    label: 'Allow',
    description:
      'Skip the remaining edge rules. Rate limits, attack mode and password protection still apply.',
    terminal: true,
  },
  {
    type: 'challenge',
    label: 'Challenge',
    description:
      'Ask the browser to solve a proof-of-work check. Visitors who pass continue to later rules.',
    terminal: true,
  },
  {
    type: 'redirect',
    label: 'Redirect',
    description: 'Send the visitor to another URL or path.',
    terminal: true,
  },
  {
    type: 'log',
    label: 'Log',
    description: 'Record the match and keep checking later rules.',
    terminal: false,
  },
]

export const ACTION_LABELS: Record<ActionType, string> = Object.fromEntries(
  ACTIONS.map((a) => [a.type, a.label])
) as Record<ActionType, string>

/** Limits from ADR-052 §1; the backend enforces the same values. */
export const LIMITS = {
  rulesPerProject: 100,
  conditionsPerRule: 32,
  depth: 4,
  listValues: 64,
  cidrValues: 1000,
  valueBytes: 256,
  globWildcards: 8,
  nameLength: 80,
  blockBodyBytes: 1024,
} as const
