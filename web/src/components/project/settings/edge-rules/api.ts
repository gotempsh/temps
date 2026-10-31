// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Conversion between the API's rule format (crates/temps-edge-rules) and the
// editor's draft types. The API keeps list values as arrays; the editor keeps
// one string per condition so typing a comma never loses input.

import { LIST_OPERATORS, PRESENCE_OPERATORS } from './fields'
import { splitList } from './expression'
import type { SampleRequest } from './evaluate'
import type {
  Combinator,
  Condition,
  ConditionGroup,
  EdgeRule,
  EdgeRuleDraft,
  Expression,
  FieldId,
  Operator,
  RedirectStatus,
  RuleAction,
  RuleMode,
} from './types'

export interface ApiCondition {
  kind: 'condition'
  id?: string
  field: FieldId
  key?: string | null
  op: Operator
  values?: string[]
  case_insensitive?: boolean
}

export interface ApiGroup {
  kind?: 'group'
  id?: string
  combinator: Combinator
  children?: ApiExpression[]
}

export type ApiExpression = ApiCondition | (ApiGroup & { kind: 'group' })

export type ApiAction =
  | { type: 'block'; status: number; body?: string }
  | { type: 'allow' }
  | { type: 'challenge' }
  | { type: 'redirect'; target: string; status: number }
  | { type: 'log' }

export interface ApiRule {
  id: number
  revision: number
  name: string
  enabled: boolean
  mode: RuleMode
  environment_id?: number | null
  expression: ApiGroup
  action: ApiAction
  created_at: string
  updated_at: string
}

export interface ApiRuleInput {
  name: string
  enabled: boolean
  mode: RuleMode
  environment_id: number | null
  expression: ApiGroup
  action: ApiAction
}

let counter = 0
const freshId = () => `a${(counter += 1)}`

function conditionFromApi(c: ApiCondition): Condition {
  const values = c.values ?? []
  return {
    kind: 'condition',
    id: c.id || freshId(),
    field: c.field,
    key: c.key ?? undefined,
    op: c.op,
    value: LIST_OPERATORS.has(c.op) ? values.join(', ') : (values[0] ?? ''),
    caseInsensitive: c.case_insensitive ?? false,
  }
}

export function groupFromApi(g: ApiGroup): ConditionGroup {
  return {
    kind: 'group',
    id: g.id || freshId(),
    combinator: g.combinator,
    children: (g.children ?? []).map((child): Expression =>
      child.kind === 'group' ? groupFromApi(child) : conditionFromApi(child)
    ),
  }
}

function conditionToApi(c: Condition): ApiCondition {
  const values = PRESENCE_OPERATORS.has(c.op)
    ? []
    : LIST_OPERATORS.has(c.op)
      ? splitList(c.value)
      : [c.value.trim()].filter(Boolean)
  const out: ApiCondition = {
    kind: 'condition',
    id: c.id,
    field: c.field,
    op: c.op,
    values,
    case_insensitive: !!c.caseInsensitive,
  }
  if (c.key !== undefined) out.key = c.key.trim()
  return out
}

export function groupToApi(g: ConditionGroup): ApiGroup & { kind: 'group' } {
  return {
    kind: 'group',
    id: g.id,
    combinator: g.combinator,
    children: g.children.map((child) =>
      child.kind === 'group' ? groupToApi(child) : conditionToApi(child)
    ),
  }
}

export function actionFromApi(a: ApiAction): RuleAction {
  switch (a.type) {
    case 'block':
      return { type: 'block', status: a.status, body: a.body ?? '' }
    case 'redirect':
      return {
        type: 'redirect',
        target: a.target,
        status: a.status as RedirectStatus,
      }
    case 'allow':
    case 'challenge':
    case 'log':
      return { type: a.type }
  }
}

export function actionToApi(a: RuleAction): ApiAction {
  return a.type === 'block'
    ? { type: 'block', status: a.status, body: a.body }
    : a
}

export function ruleFromApi(r: ApiRule): EdgeRule {
  return {
    id: String(r.id),
    revision: r.revision,
    name: r.name,
    enabled: r.enabled,
    mode: r.mode,
    environmentId: r.environment_id ?? null,
    expression: groupFromApi(r.expression),
    action: actionFromApi(r.action),
    createdAt: r.created_at,
    updatedAt: r.updated_at,
  }
}

export function draftToApi(d: EdgeRuleDraft): ApiRuleInput {
  return {
    name: d.name.trim(),
    enabled: d.enabled,
    mode: d.mode,
    environment_id: d.environmentId,
    expression: groupToApi(d.expression),
    action: actionToApi(d.action),
  }
}

/** The API's dry-run request (`SampleRequest` in the engine). */
export interface ApiSampleRequest {
  method?: string
  url: string
  headers?: Array<{ name: string; value: string }>
  user_agent?: string | null
  ip?: string | null
  country?: string | null
  continent?: string | null
  asn?: number | null
  is_hosting_provider?: boolean | null
  ja4?: string | null
  bot_class?: string | null
  environment_id?: number | null
  challenge_cleared?: boolean
}

export function sampleFromApi(s: ApiSampleRequest): SampleRequest {
  return {
    method: s.method ?? 'GET',
    url: s.url,
    ip: s.ip ?? '',
    country: s.country ?? '',
    continent: s.continent ?? '',
    asn: s.asn == null ? '' : String(s.asn),
    isHostingProvider: s.is_hosting_provider ?? false,
    userAgent: s.user_agent ?? '',
    headers: (s.headers ?? []).map((h) => `${h.name}: ${h.value}`).join('\n'),
    ja4: s.ja4 ?? '',
    botClass: s.bot_class ?? '',
    environmentId: s.environment_id ?? null,
    challengeCleared: s.challenge_cleared ?? false,
  }
}

export function sampleToApi(s: SampleRequest): ApiSampleRequest {
  const headers = s.headers
    .split('\n')
    .map((line) => {
      const colon = line.indexOf(':')
      return colon > 0
        ? {
            name: line.slice(0, colon).trim(),
            value: line.slice(colon + 1).trim(),
          }
        : null
    })
    .filter((h): h is { name: string; value: string } => h !== null)
  const asn = Number(s.asn.trim().replace(/^AS/i, ''))
  return {
    method: s.method,
    url: s.url,
    headers,
    user_agent: s.userAgent.trim() || null,
    ip: s.ip.trim() || null,
    country: s.country.trim() || null,
    continent: s.continent || null,
    asn: s.asn.trim() && Number.isFinite(asn) ? asn : null,
    is_hosting_provider: s.isHostingProvider,
    ja4: s.ja4.trim() || null,
    bot_class: s.botClass || null,
    environment_id: s.environmentId,
    challenge_cleared: s.challengeCleared,
  }
}
