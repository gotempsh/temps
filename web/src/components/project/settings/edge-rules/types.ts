// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Edge rules data model (ADR-052). These mirror the shapes the backend will
// serve; until it exists, `store.ts` persists them in the browser.

export type FieldId =
  | 'ip'
  | 'country'
  | 'continent'
  | 'asn'
  | 'is_hosting_provider'
  | 'host'
  | 'path'
  | 'raw_path'
  | 'method'
  | 'query'
  | 'header'
  | 'cookie'
  | 'user_agent'
  | 'ja4'
  | 'bot_class'

export type Operator =
  | 'eq'
  | 'in'
  | 'in_cidr'
  | 'starts_with'
  | 'ends_with'
  | 'contains'
  | 'glob'
  | 'regex'
  | 'exists'
  | 'not_exists'

/** A single test. List operators (`in`, `in_cidr`) keep a comma- or
 * newline-separated string in `value`; it is split when validated or
 * evaluated so typing a trailing comma never loses input. */
export interface Condition {
  kind: 'condition'
  id: string
  field: FieldId
  /** Header, query parameter or cookie name. */
  key?: string
  op: Operator
  value: string
  caseInsensitive?: boolean
}

/** `none` serializes to `not(any(...))` in the ADR's AST. */
export type Combinator = 'all' | 'any' | 'none'

export interface ConditionGroup {
  kind: 'group'
  id: string
  combinator: Combinator
  children: Expression[]
}

export type Expression = Condition | ConditionGroup

export type RedirectStatus = 301 | 302 | 307 | 308

export type RuleAction =
  | { type: 'block'; status: number; body: string }
  | { type: 'allow' }
  | { type: 'challenge' }
  | { type: 'redirect'; target: string; status: RedirectStatus }
  | { type: 'log' }

export type ActionType = RuleAction['type']

export type RuleMode = 'enforce' | 'log_only'

/** The editable part of a rule. */
export interface EdgeRuleDraft {
  name: string
  enabled: boolean
  mode: RuleMode
  /** `null` applies the rule to every environment of the project. */
  environmentId: number | null
  expression: ConditionGroup
  action: RuleAction
}

export interface EdgeRule extends EdgeRuleDraft {
  id: string
  /** Bumped on every write; updates must send the revision they edited. */
  revision: number
  createdAt: string
  updatedAt: string
}
