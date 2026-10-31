// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Construction, validation and plain-English description of edge rules.
// Evaluation lives in `evaluate.ts`.

import {
  ACTION_LABELS,
  fieldMeta,
  LIMITS,
  LIST_OPERATORS,
  OPERATOR_LABELS,
  PRESENCE_OPERATORS,
  SPOOFABLE_HEADERS,
} from './fields'
import { parseAsn, parseCidr } from './ip'
import type {
  Condition,
  ConditionGroup,
  EdgeRuleDraft,
  Expression,
  FieldId,
  RuleAction,
} from './types'

let idCounter = 0
export function newNodeId(): string {
  idCounter += 1
  return `n${Date.now().toString(36)}${idCounter.toString(36)}`
}

/** Paths ignore case by default: an upstream that treats /ADMIN like /admin
 * would otherwise turn a case-sensitive block into a bypass. */
export function defaultCaseInsensitive(field: FieldId): boolean | undefined {
  if (!fieldMeta(field).caseFlag) return undefined
  return field === 'path' || field === 'raw_path'
}

export function newCondition(field: FieldId = 'path'): Condition {
  const meta = fieldMeta(field)
  return {
    kind: 'condition',
    id: newNodeId(),
    field,
    op: meta.operators[0],
    value: meta.valueKind === 'boolean' ? 'true' : '',
    key: meta.keyed ? '' : undefined,
    caseInsensitive: defaultCaseInsensitive(field),
  }
}

export function newGroup(
  combinator: ConditionGroup['combinator'] = 'all',
  children: Expression[] = [newCondition()]
): ConditionGroup {
  return { kind: 'group', id: newNodeId(), combinator, children }
}

export function emptyDraft(): EdgeRuleDraft {
  return {
    name: '',
    enabled: true,
    mode: 'log_only',
    environmentId: null,
    expression: newGroup('all'),
    action: { type: 'block', status: 403, body: '' },
  }
}

/** Splits a list value on commas and newlines, dropping blanks. */
export function splitList(value: string): string[] {
  return value
    .split(/[\n,]/)
    .map((v) => v.trim())
    .filter(Boolean)
}

export function walk(
  expr: Expression,
  visit: (node: Expression, depth: number) => void,
  depth = 1
): void {
  visit(expr, depth)
  if (expr.kind === 'group') {
    for (const child of expr.children) walk(child, visit, depth + 1)
  }
}

export function conditionsOf(expr: Expression): Condition[] {
  const out: Condition[] = []
  walk(expr, (node) => {
    if (node.kind === 'condition') out.push(node)
  })
  return out
}

export function referencesField(expr: Expression, field: FieldId): boolean {
  return conditionsOf(expr).some((c) => c.field === field)
}

// ---------------------------------------------------------------------------
// Tree edits (immutable)
// ---------------------------------------------------------------------------

export function updateNode(
  root: ConditionGroup,
  id: string,
  update: (node: Expression) => Expression
): ConditionGroup {
  const visit = (node: Expression): Expression => {
    if (node.id === id) return update(node)
    if (node.kind === 'group') {
      return { ...node, children: node.children.map(visit) }
    }
    return node
  }
  return visit(root) as ConditionGroup
}

export function removeNode(root: ConditionGroup, id: string): ConditionGroup {
  const visit = (group: ConditionGroup): ConditionGroup => ({
    ...group,
    children: group.children
      .filter((child) => child.id !== id)
      .map((child) => (child.kind === 'group' ? visit(child) : child)),
  })
  return visit(root)
}

/** Resets operator and value when the field changes to one that does not
 * support them. */
export function changeField(condition: Condition, field: FieldId): Condition {
  const meta = fieldMeta(field)
  const op = meta.operators.includes(condition.op)
    ? condition.op
    : meta.operators[0]
  return {
    ...condition,
    field,
    op,
    key: meta.keyed ? (condition.key ?? '') : undefined,
    value: meta.valueKind === 'boolean' ? 'true' : '',
    caseInsensitive: defaultCaseInsensitive(field),
  }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/** `target` is `name`, `action`, or the id of the offending node. */
export interface RuleIssue {
  target: string
  message: string
}

const HEADER_NAME = /^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/
const HOSTNAME =
  /^(\*\.)?[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)*$/i
// The proxy compiles patterns with Rust's linear-time `regex` crate, which
// has no look-around or back-references.
const UNSUPPORTED_REGEX = /\(\?<?[=!]|\\[1-9]|\\k</
const PLACEHOLDER = /\{([^}]*)\}/g

const byteLength = (s: string) => new TextEncoder().encode(s).length

/**
 * Only what Rust's `regex` certainly refuses. The syntax itself is not
 * checked here: JavaScript's RegExp rejects valid Rust patterns such as
 * `(?i)admin` or `(?P<area>admin)`, which would block saving a rule the proxy
 * accepts. The server validates every pattern on save and its error explains
 * a typo.
 */
function validateRegex(source: string): string | null {
  if (UNSUPPORTED_REGEX.test(source)) {
    return 'Look-ahead, look-behind and back-references are not supported.'
  }
  return null
}

function validateCondition(c: Condition): string[] {
  const meta = fieldMeta(c.field)
  const issues: string[] = []
  if (!meta.operators.includes(c.op)) {
    issues.push(
      `"${OPERATOR_LABELS[c.op]}" is not available for ${meta.label}.`
    )
    return issues
  }
  if (meta.keyed) {
    const key = (c.key ?? '').trim()
    if (!key) issues.push(`Enter the ${meta.keyed} name.`)
    else if (meta.keyed === 'header' && !HEADER_NAME.test(key))
      issues.push(`"${key}" is not a valid header name.`)
    else if (/\s/.test(key))
      issues.push(`The ${meta.keyed} name cannot contain spaces.`)
  }
  if (PRESENCE_OPERATORS.has(c.op)) return issues

  const values = LIST_OPERATORS.has(c.op)
    ? splitList(c.value)
    : [c.value.trim()].filter(Boolean)
  if (values.length === 0) {
    issues.push('Enter a value.')
    return issues
  }
  const max = c.op === 'in_cidr' ? LIMITS.cidrValues : LIMITS.listValues
  if (values.length > max) issues.push(`Use at most ${max} values.`)
  for (const v of values) {
    if (byteLength(v) > LIMITS.valueBytes) {
      issues.push(`Values are limited to ${LIMITS.valueBytes} bytes.`)
      break
    }
  }

  const invalid = (test: (v: string) => boolean, what: string) => {
    const bad = values.filter((v) => !test(v))
    if (bad.length)
      issues.push(
        `${bad.slice(0, 3).join(', ')} ${bad.length === 1 ? 'is' : 'are'} not ${what}.`
      )
  }
  switch (meta.valueKind) {
    case 'cidr':
      invalid((v) => parseCidr(v) !== null, 'a valid IP address or CIDR range')
      break
    case 'number':
      invalid((v) => parseAsn(v) !== null, 'an AS number')
      break
    case 'boolean':
      invalid((v) => v === 'true' || v === 'false', 'yes or no')
      break
    case 'options': {
      const allowed = new Set(meta.options?.map((o) => o.value))
      invalid((v) => allowed.has(v), `a valid ${meta.label.toLowerCase()}`)
      break
    }
    case 'text':
      if (c.field === 'country')
        invalid((v) => /^[A-Z]{2}$/i.test(v), 'a two-letter country code')
      if (c.field === 'host' && (c.op === 'eq' || c.op === 'in'))
        invalid((v) => HOSTNAME.test(v), 'a hostname')
      if (
        (c.field === 'path' || c.field === 'raw_path') &&
        (c.op === 'eq' || c.op === 'in' || c.op === 'starts_with')
      )
        invalid((v) => v.startsWith('/'), 'a path starting with /')
      break
  }
  if (c.op === 'glob') {
    const stars = (c.value.match(/\*/g) ?? []).length
    if (stars > LIMITS.globWildcards)
      issues.push(
        `Patterns may use at most ${LIMITS.globWildcards} * wildcards.`
      )
  }
  if (c.op === 'regex') {
    const problem = validateRegex(c.value)
    if (problem) issues.push(problem)
  }
  return issues
}

function validateAction(action: RuleAction): string[] {
  switch (action.type) {
    case 'block': {
      const issues: string[] = []
      if (
        !Number.isInteger(action.status) ||
        action.status < 400 ||
        action.status > 499
      )
        issues.push('Block status must be between 400 and 499.')
      if (byteLength(action.body) > LIMITS.blockBodyBytes)
        issues.push(
          `The response body is limited to ${LIMITS.blockBodyBytes} bytes.`
        )
      return issues
    }
    case 'redirect': {
      const target = action.target.trim()
      if (!target) return ['Enter where to redirect to.']
      const issues: string[] = []
      for (const [, name] of target.matchAll(PLACEHOLDER)) {
        if (name !== 'path' && name !== 'query')
          issues.push(`Unknown placeholder {${name}}. Use {path} or {query}.`)
      }
      // The visitor controls {path} and {query}. Neither may be able to
      // change the destination host, or the rule becomes an open redirect.
      const url = /^https?:\/\/([^/?#\s{}\\]+)(.*)$/i.exec(target)
      if (url) {
        const rest = url[2]
        if (rest && !/^([/?#]|\{path\})/.test(rest))
          issues.push(
            'The host must be followed by /, ?, # or {path}, so a visitor cannot change where the redirect goes.'
          )
      } else if (!target.startsWith('/')) {
        issues.push(
          'Use a path starting with / or an absolute http(s) URL with a fixed host.'
        )
      } else if (/^\/[/\\]|^\/\{path\}/.test(target)) {
        issues.push(
          'A path target cannot start with //, /\\ or /{path}: browsers treat those as a different host.'
        )
      }
      const queryAt = target.indexOf('{query}')
      if (queryAt !== -1) {
        const questionAt = target.indexOf('?')
        if (questionAt === -1 || questionAt > queryAt)
          issues.push(
            'Put {query} after a ? in the target, for example /new?{query}.'
          )
      }
      return issues
    }
    case 'allow':
    case 'challenge':
    case 'log':
      return []
  }
}

export function validateRule(draft: EdgeRuleDraft): RuleIssue[] {
  const issues: RuleIssue[] = []
  const name = draft.name.trim()
  if (!name) issues.push({ target: 'name', message: 'Give the rule a name.' })
  else if (name.length > LIMITS.nameLength)
    issues.push({
      target: 'name',
      message: `Names are limited to ${LIMITS.nameLength} characters.`,
    })

  let conditions = 0
  walk(draft.expression, (node, depth) => {
    if (node.kind === 'group') {
      if (depth > LIMITS.depth)
        issues.push({
          target: node.id,
          message: `Groups can be nested at most ${LIMITS.depth} levels deep.`,
        })
      if (node.children.length === 0 && node !== draft.expression)
        issues.push({
          target: node.id,
          message: 'Add a condition to this group or remove it.',
        })
      return
    }
    conditions += 1
    for (const message of validateCondition(node))
      issues.push({ target: node.id, message })
  })
  if (conditions > LIMITS.conditionsPerRule)
    issues.push({
      target: draft.expression.id,
      message: `A rule can have at most ${LIMITS.conditionsPerRule} conditions.`,
    })

  for (const message of validateAction(draft.action))
    issues.push({ target: 'action', message })
  return issues
}

/** Non-blocking advice about a valid rule. */
export function ruleWarnings(draft: EdgeRuleDraft): string[] {
  const warnings: string[] = []
  const conditions = conditionsOf(draft.expression)
  const action = draft.action.type
  if (conditions.length === 0) {
    warnings.push(
      'This rule has no conditions, so it matches every request to the project.'
    )
  }
  const spoofable = conditions.filter(
    (c) =>
      c.field === 'header' &&
      SPOOFABLE_HEADERS.has((c.key ?? '').trim().toLowerCase())
  )
  if (spoofable.length) {
    warnings.push(
      `${spoofable[0].key} is set by the client and can be forged. Use the Client IP field, which Temps resolves through trusted proxies.`
    )
  }
  const pathConditions = conditions.filter((c) => c.field === 'path')
  if (action === 'allow' && pathConditions.some((c) => c.caseInsensitive)) {
    warnings.push(
      'Ignoring case on an Allow rule widens what it lets through: /ADMIN and /Admin are allowed too.'
    )
  }
  if (
    (action === 'block' || action === 'challenge') &&
    pathConditions.some(
      (c) =>
        !c.caseInsensitive &&
        /[a-z]/i.test(c.value) &&
        !PRESENCE_OPERATORS.has(c.op)
    )
  ) {
    warnings.push(
      'Path tests are case-sensitive, so /ADMIN does not match /admin. If your app treats paths case-insensitively, turn on "Ignore case".'
    )
  }
  if (action === 'allow' && pathConditions.length) {
    warnings.push(
      'Allow rules that test the path never match an ambiguous path such as //admin or /%61dmin. Those requests continue to later rules.'
    )
  }
  if (conditions.some((c) => fieldMeta(c.field).geo)) {
    warnings.push(
      'Country, continent and network conditions need the GeoIP database. Without it they never match.'
    )
  }
  if (action === 'challenge') {
    warnings.push(
      'Challenges need HTTPS: plain HTTP requests that match are refused with 426 Upgrade Required.'
    )
  }
  return warnings
}

// ---------------------------------------------------------------------------
// Description
// ---------------------------------------------------------------------------

function describeValue(c: Condition): string {
  const meta = fieldMeta(c.field)
  const values = LIST_OPERATORS.has(c.op)
    ? splitList(c.value)
    : [c.value.trim()]
  const label = (v: string) =>
    meta.options?.find((o) => o.value === v)?.label ?? v
  const shown = values.slice(0, 3).map(label)
  const more = values.length > 3 ? ` +${values.length - 3} more` : ''
  return shown.join(', ') + more
}

export function describeCondition(c: Condition): string {
  const meta = fieldMeta(c.field)
  if (meta.valueKind === 'boolean') {
    return c.value === 'true'
      ? `request comes from a ${meta.label.toLowerCase()}`
      : `request does not come from a ${meta.label.toLowerCase()}`
  }
  const subject = meta.keyed
    ? `${meta.keyed} ${c.key?.trim() || '…'}`
    : meta.label.toLowerCase()
  if (PRESENCE_OPERATORS.has(c.op)) {
    return `${subject} ${OPERATOR_LABELS[c.op]}`
  }
  const value = describeValue(c) || '…'
  const ci = c.caseInsensitive ? ' (ignoring case)' : ''
  return `${subject} ${OPERATOR_LABELS[c.op]} ${value}${ci}`
}

export function describeExpression(expr: Expression, nested = false): string {
  if (expr.kind === 'condition') return describeCondition(expr)
  if (expr.children.length === 0) return 'every request'
  const parts = expr.children.map((child) => describeExpression(child, true))
  let text: string
  if (expr.combinator === 'none') {
    text = `none of (${parts.join('; ')})`
  } else {
    text = parts.join(expr.combinator === 'all' ? ' and ' : ' or ')
    if (nested && parts.length > 1) text = `(${text})`
  }
  return text
}

/**
 * Name used when the rule is saved without one, so naming is optional:
 * "Block when path starts with /admin and 1 more condition".
 */
export function autoName(
  draft: Pick<EdgeRuleDraft, 'expression' | 'action'>
): string {
  const conditions = conditionsOf(draft.expression)
  const verb = ACTION_LABELS[draft.action.type]
  let what: string
  if (conditions.length === 0) what = 'every request'
  else if (conditions.length === 1 || draft.expression.combinator === 'none')
    what = describeExpression(draft.expression)
  else {
    const rest = conditions.length - 1
    what = `${describeCondition(conditions[0])} and ${rest} more ${rest === 1 ? 'condition' : 'conditions'}`
  }
  const name = `${verb} when ${what}`
  return name.length <= LIMITS.nameLength
    ? name
    : `${name.slice(0, LIMITS.nameLength - 1)}…`
}

/** The draft with an automatic name filled in when the name is blank. */
export function withAutoName(draft: EdgeRuleDraft): EdgeRuleDraft {
  return draft.name.trim() ? draft : { ...draft, name: autoName(draft) }
}

export function describeAction(action: RuleAction): string {
  switch (action.type) {
    case 'block':
      return `Block with ${action.status}`
    case 'redirect':
      return `Redirect (${action.status}) to ${action.target || '…'}`
    default:
      return ACTION_LABELS[action.type]
  }
}
