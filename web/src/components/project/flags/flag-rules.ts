// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Targeting rules and variants as the console edits them.
 *
 * The server (`crates/temps-flags/src/services/validation.rs`) is the authority
 * and rejects anything invalid with a message naming the rule and condition.
 * The checks here mirror it so the editor can point at the field before a
 * request is made; when they disagree the server wins and its message is shown.
 */

import {
  parseFlagValue,
  toEditableString,
  type FlagValueType,
} from './flag-value'

export type ConditionOperator =
  | 'eq'
  | 'neq'
  | 'in'
  | 'not_in'
  | 'contains'
  | 'not_contains'
  | 'matches'
  | 'gt'
  | 'gte'
  | 'lt'
  | 'lte'
  | 'exists'
  | 'not_exists'

export type AttributeValue = string | number | boolean

export interface RuleCondition {
  attribute: string
  operator: ConditionOperator
  value?: AttributeValue | AttributeValue[] | null
}

export interface VariantWeight {
  variant: string
  weight: number
}

export type RuleServe =
  | { kind: 'value'; value: unknown }
  | { kind: 'variant'; variant: string }
  | { kind: 'split'; split: VariantWeight[] }

export interface FlagRule {
  description?: string | null
  conditions: RuleCondition[]
  rollout_percentage?: number | null
  serve: RuleServe
}

export interface FlagVariant {
  key: string
  value: unknown
  description?: string | null
}

/** Limits enforced by the server; mirrored so the editor stops at them. */
export const MAX_RULES = 50
export const MAX_CONDITIONS = 20
export const MAX_VARIANTS = 20
export const MAX_LIST_VALUES = 100

type OperandShape = 'none' | 'scalar' | 'string' | 'number' | 'list'

export const OPERATORS: {
  value: ConditionOperator
  label: string
  operand: OperandShape
}[] = [
  { value: 'eq', label: 'equals', operand: 'scalar' },
  { value: 'neq', label: 'does not equal', operand: 'scalar' },
  { value: 'in', label: 'is one of', operand: 'list' },
  { value: 'not_in', label: 'is not one of', operand: 'list' },
  { value: 'contains', label: 'contains', operand: 'string' },
  { value: 'not_contains', label: 'does not contain', operand: 'string' },
  { value: 'matches', label: 'matches regex', operand: 'string' },
  { value: 'gt', label: '>', operand: 'number' },
  { value: 'gte', label: '≥', operand: 'number' },
  { value: 'lt', label: '<', operand: 'number' },
  { value: 'lte', label: '≤', operand: 'number' },
  { value: 'exists', label: 'is set', operand: 'none' },
  { value: 'not_exists', label: 'is not set', operand: 'none' },
]

export function operandShape(operator: ConditionOperator): OperandShape {
  return OPERATORS.find((o) => o.value === operator)?.operand ?? 'scalar'
}

export function operatorLabel(operator: ConditionOperator): string {
  return OPERATORS.find((o) => o.value === operator)?.label ?? operator
}

/**
 * Attributes the public endpoint fills in from the request itself, so a rule
 * can target them without the app sending anything. Offered as suggestions in
 * the attribute field.
 */
export const IMPLICIT_ATTRIBUTES = [
  'environment',
  'host',
  'country',
  'browser',
  'os',
  'device_type',
  'visitor_id',
  'session_id',
] as const

/**
 * Turn the text typed into a condition's value box into the operand the
 * operator needs. Lists are comma-separated; a token that reads as a number or
 * boolean is sent as one, because the evaluator compares `5` and `"5"` as
 * equal but `true` and `"true"` as different.
 */
export function parseOperand(
  operator: ConditionOperator,
  raw: string
): { ok: true; value: RuleCondition['value'] } | { ok: false; error: string } {
  const shape = operandShape(operator)
  const trimmed = raw.trim()
  switch (shape) {
    case 'none':
      return { ok: true, value: undefined }
    case 'number': {
      if (trimmed === '') return { ok: false, error: 'Enter a number.' }
      const n = Number(trimmed)
      if (!Number.isFinite(n)) return { ok: false, error: 'Not a number.' }
      return { ok: true, value: n }
    }
    case 'string':
      if (trimmed === '') return { ok: false, error: 'Enter a value.' }
      if (operator === 'matches') {
        try {
          new RegExp(trimmed, 'u')
        } catch {
          return { ok: false, error: 'Not a valid regular expression.' }
        }
      }
      return { ok: true, value: trimmed }
    case 'scalar':
      if (trimmed === '') return { ok: false, error: 'Enter a value.' }
      return { ok: true, value: scalar(trimmed) }
    case 'list': {
      const items = trimmed
        .split(',')
        .map((item) => item.trim())
        .filter((item) => item !== '')
      if (items.length === 0) {
        return { ok: false, error: 'Enter at least one value.' }
      }
      if (items.length > MAX_LIST_VALUES) {
        return {
          ok: false,
          error: `At most ${MAX_LIST_VALUES} values.`,
        }
      }
      return { ok: true, value: items.map(scalar) }
    }
  }
}

function scalar(token: string): AttributeValue {
  if (token === 'true') return true
  if (token === 'false') return false
  if (/^-?\d+(\.\d+)?$/.test(token)) return Number(token)
  return token
}

/** The text a condition's value box shows for a stored operand. */
export function operandText(value: RuleCondition['value']): string {
  if (value === null || value === undefined) return ''
  if (Array.isArray(value)) return value.map(String).join(', ')
  return String(value)
}

/** A one-line, human reading of a rule, for the collapsed rule list. */
export function describeRule(rule: FlagRule, type: FlagValueType): string {
  const conditions =
    rule.conditions.length === 0
      ? 'Everyone'
      : rule.conditions
          .map((condition) => {
            const shape = operandShape(condition.operator)
            const operand =
              shape === 'none' ? '' : ` ${operandText(condition.value)}`
            return `${condition.attribute} ${operatorLabel(condition.operator)}${operand}`
          })
          .join(' and ')
  const rollout =
    rule.rollout_percentage !== null &&
    rule.rollout_percentage !== undefined &&
    rule.rollout_percentage < 100
      ? ` (${rule.rollout_percentage}% of them)`
      : ''
  return `${conditions}${rollout} → ${describeServe(rule.serve, type)}`
}

export function describeServe(serve: RuleServe, type: FlagValueType): string {
  switch (serve.kind) {
    case 'value':
      if (type === 'bool') return serve.value === true ? 'On' : 'Off'
      return typeof serve.value === 'string'
        ? serve.value
        : JSON.stringify(serve.value)
    case 'variant':
      return `variant ${serve.variant}`
    case 'split':
      return serve.split
        .map((arm) => `${arm.variant} ${arm.weight}%`)
        .join(' / ')
  }
}

/** Variant keys a set of rules serves, directly or in a split. */
export function referencedVariants(rules: FlagRule[]): Set<string> {
  const used = new Set<string>()
  for (const rule of rules) {
    if (rule.serve.kind === 'variant') used.add(rule.serve.variant)
    if (rule.serve.kind === 'split') {
      for (const arm of rule.serve.split) used.add(arm.variant)
    }
  }
  return used
}

/**
 * Check one rule against the declared variants. Returns the first problem, in
 * words that name the field, or null.
 */
export function validateRule(
  rule: FlagRule,
  variants: FlagVariant[]
): string | null {
  if (rule.conditions.length > MAX_CONDITIONS) {
    return `A rule can have at most ${MAX_CONDITIONS} conditions.`
  }
  for (const [index, condition] of rule.conditions.entries()) {
    if (condition.attribute.trim() === '') {
      return `Condition ${index + 1} needs an attribute.`
    }
  }
  const rollout = rule.rollout_percentage
  if (
    rollout !== null &&
    rollout !== undefined &&
    (!Number.isFinite(rollout) || rollout < 0 || rollout > 100)
  ) {
    return 'Rollout must be between 0 and 100.'
  }
  const declared = new Set(variants.map((variant) => variant.key))
  switch (rule.serve.kind) {
    case 'value':
      return null
    case 'variant':
      return declared.has(rule.serve.variant)
        ? null
        : `Variant "${rule.serve.variant}" is not declared on this flag.`
    case 'split': {
      if (rule.serve.split.length < 2) {
        return 'A split needs at least two variants.'
      }
      const seen = new Set<string>()
      let total = 0
      for (const arm of rule.serve.split) {
        if (!declared.has(arm.variant)) {
          return `Variant "${arm.variant}" is not declared on this flag.`
        }
        if (seen.has(arm.variant)) {
          return `Variant "${arm.variant}" appears twice in the split.`
        }
        if (!Number.isInteger(arm.weight) || arm.weight < 0) {
          return 'Split weights must be whole numbers.'
        }
        seen.add(arm.variant)
        total += arm.weight
      }
      return total === 100 ? null : `Split weights add up to ${total}, not 100.`
    }
  }
}

/**
 * `split` with `variant` set to `weight`. Subjects are bucketed into ranges
 * laid out in the split's own order, so the saved arms keep that order and
 * only the edited weight changes: moving 1% moves about 1% of subjects. A
 * variant not yet in the split is appended; arms left at 0 are dropped
 * (they hold no range), except the one being edited.
 */
export function setSplitWeight(
  split: VariantWeight[],
  variant: string,
  weight: number
): VariantWeight[] {
  const present = split.some((arm) => arm.variant === variant)
  const next = split.map((arm) =>
    arm.variant === variant ? { variant, weight } : arm
  )
  if (!present) next.push({ variant, weight })
  return next.filter((arm) => arm.weight > 0 || arm.variant === variant)
}

/** An even split across `keys`, remainder on the first arms. */
export function evenSplit(keys: string[]): VariantWeight[] {
  if (keys.length === 0) return []
  const base = Math.floor(100 / keys.length)
  let remainder = 100 - base * keys.length
  return keys.map((variant) => {
    const extra = remainder > 0 ? 1 : 0
    remainder -= extra
    return { variant, weight: base + extra }
  })
}

/** Variant keys follow the flag key rule, shorter. */
export function validateVariantKey(key: string): string | null {
  if (!key) return 'Enter a key.'
  if (key.length > 64) return 'Variant keys are limited to 64 characters.'
  if (!/^[a-z0-9][a-z0-9._-]*$/.test(key)) {
    return 'Lowercase letters, digits, dot, underscore and hyphen; start with a letter or digit.'
  }
  return null
}

/** A fresh rule that serves the flag's default to everyone. */
export function newRule(defaultValue: unknown): FlagRule {
  return {
    conditions: [{ attribute: '', operator: 'eq', value: '' }],
    rollout_percentage: null,
    serve: { kind: 'value', value: defaultValue },
  }
}

/** Parse `key=value` lines from the test panel into attributes. */
export function parseAttributeLines(
  text: string
):
  | { ok: true; attributes: Record<string, AttributeValue> }
  | { ok: false; error: string } {
  const attributes: Record<string, AttributeValue> = {}
  for (const [index, line] of text.split('\n').entries()) {
    const trimmed = line.trim()
    if (trimmed === '') continue
    const separator = trimmed.indexOf('=')
    if (separator <= 0) {
      return {
        ok: false,
        error: `Line ${index + 1}: write it as name=value.`,
      }
    }
    const name = trimmed.slice(0, separator).trim()
    attributes[name] = scalar(trimmed.slice(separator + 1).trim())
  }
  return { ok: true, attributes }
}

/** What an evaluation's reason means, for the test panel. */
export function describeReason(reason: {
  kind: string
  index?: number
}): string {
  switch (reason.kind) {
    case 'RULE_MATCH':
      return `Rule ${(reason.index ?? 0) + 1} matched`
    case 'PERCENTAGE_ROLLOUT':
      return `Rule ${(reason.index ?? 0) + 1} matched and the subject is in its rollout`
    case 'ENVIRONMENT_VALUE':
      return "No rule matched; served the environment's value"
    case 'DEFAULT':
      return 'No rule matched; served the default'
    case 'DISABLED':
      return 'Disabled in this environment; served the default'
    case 'FLAG_NOT_FOUND':
      return 'The flag does not exist or is archived'
    case 'ERROR':
      return 'The stored rules could not be evaluated; served the default'
    default:
      return reason.kind
  }
}

// ---------------------------------------------------------------------------
// Editor drafts
// ---------------------------------------------------------------------------

/** A condition as typed: the operand stays text until the rule is saved. */
export interface DraftCondition {
  attribute: string
  operator: ConditionOperator
  text: string
  /**
   * The stored operand this draft was opened with. Text cannot carry a
   * string `"true"` or a list item containing a comma, so an operand the user
   * did not edit is saved back exactly as stored rather than re-parsed.
   */
  stored?: {
    operator: ConditionOperator
    text: string
    value: RuleCondition['value']
  }
}

/** A rule as the editor holds it. */
export interface DraftRule {
  /** Stable React key; never sent. */
  id: string
  description: string
  conditions: DraftCondition[]
  /** Empty means "everyone who matches". */
  rollout: string
  serveKind: RuleServe['kind']
  valueText: string
  variant: string
  split: VariantWeight[]
}

let draftSequence = 0
function nextDraftId(): string {
  draftSequence += 1
  return `rule-${draftSequence}`
}

export function toDraft(rule: FlagRule, type: FlagValueType): DraftRule {
  return {
    id: nextDraftId(),
    description: rule.description ?? '',
    conditions: rule.conditions.map((condition) => ({
      attribute: condition.attribute,
      operator: condition.operator,
      text: operandText(condition.value),
      stored: {
        operator: condition.operator,
        text: operandText(condition.value),
        value: condition.value,
      },
    })),
    rollout:
      rule.rollout_percentage === null || rule.rollout_percentage === undefined
        ? ''
        : String(rule.rollout_percentage),
    serveKind: rule.serve.kind,
    valueText:
      rule.serve.kind === 'value'
        ? toEditableString(rule.serve.value, type)
        : '',
    variant: rule.serve.kind === 'variant' ? rule.serve.variant : '',
    split: rule.serve.kind === 'split' ? rule.serve.split : [],
  }
}

export function emptyDraft(
  type: FlagValueType,
  defaultValue: unknown
): DraftRule {
  return toDraft(newRule(defaultValue), type)
}

/**
 * Turn a draft back into a rule, or say what is wrong with it. `position` is
 * 1-based and only used in messages.
 */
export function fromDraft(
  draft: DraftRule,
  type: FlagValueType,
  variants: FlagVariant[],
  position: number
): { ok: true; rule: FlagRule } | { ok: false; error: string } {
  const conditions: RuleCondition[] = []
  for (const [index, condition] of draft.conditions.entries()) {
    const attribute = condition.attribute.trim()
    if (attribute === '') {
      return {
        ok: false,
        error: `Rule ${position}, condition ${index + 1}: choose an attribute.`,
      }
    }
    const untouched =
      condition.stored !== undefined &&
      condition.stored.operator === condition.operator &&
      condition.stored.text === condition.text
    const operand = untouched
      ? { ok: true as const, value: condition.stored?.value }
      : parseOperand(condition.operator, condition.text)
    if (!operand.ok) {
      return {
        ok: false,
        error: `Rule ${position}, condition ${index + 1}: ${operand.error}`,
      }
    }
    conditions.push(
      operand.value === undefined
        ? { attribute, operator: condition.operator }
        : { attribute, operator: condition.operator, value: operand.value }
    )
  }

  let rollout: number | null = null
  if (draft.rollout.trim() !== '') {
    rollout = Number(draft.rollout)
    if (!Number.isFinite(rollout) || rollout < 0 || rollout > 100) {
      return {
        ok: false,
        error: `Rule ${position}: rollout must be between 0 and 100.`,
      }
    }
  }

  let serve: RuleServe
  switch (draft.serveKind) {
    case 'value': {
      const parsed = parseFlagValue(draft.valueText, type)
      if (!parsed.ok) {
        return { ok: false, error: `Rule ${position}: ${parsed.error}` }
      }
      serve = { kind: 'value', value: parsed.value }
      break
    }
    case 'variant':
      serve = { kind: 'variant', variant: draft.variant }
      break
    case 'split':
      serve = { kind: 'split', split: draft.split }
      break
  }

  const rule: FlagRule = {
    conditions,
    serve,
    ...(draft.description.trim()
      ? { description: draft.description.trim() }
      : {}),
    ...(rollout === null ? {} : { rollout_percentage: rollout }),
  }
  const problem = validateRule(rule, variants)
  if (problem) return { ok: false, error: `Rule ${position}: ${problem}` }
  return { ok: true, rule }
}
