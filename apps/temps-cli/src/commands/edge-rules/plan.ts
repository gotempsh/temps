// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Pure helpers for `temps edge-rules`: parsing the rules file and planning
// `apply`, kept apart from the API calls so they can be unit tested.

/** A rule as written in a rules file (`export` output, `apply` input). */
export interface RuleSpec {
  name: string
  enabled?: boolean
  mode?: 'enforce' | 'log_only'
  environment_id?: number | null
  expression: unknown
  action: unknown
}

export interface RulesFile {
  rules: RuleSpec[]
}

/** The parts of a saved rule `apply` compares. */
export interface SavedRule {
  id: number
  revision: number
  name: string
  enabled: boolean
  mode: 'enforce' | 'log_only'
  environment_id?: number | null
  expression: unknown
  action: unknown
}

export interface ApplyPlan {
  create: RuleSpec[]
  update: Array<{ id: number; revision: number; name: string; spec: RuleSpec }>
  remove: Array<{ id: number; name: string }>
  unchanged: string[]
  /** Final order, by name; resolved to ids after creates. */
  order: string[]
}

const MODES = new Set(['enforce', 'log_only'])

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/**
 * Validates the shape of a rules file. The expression and action are checked
 * by the server, which owns the rule language; this only makes sure `apply`
 * can match rules by name and order them.
 */
export function parseRulesFile(raw: string, source: string): RulesFile {
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch (e) {
    throw new Error(`${source} is not valid JSON: ${(e as Error).message}`)
  }
  const list = Array.isArray(parsed) ? parsed : isObject(parsed) ? parsed.rules : undefined
  if (!Array.isArray(list)) {
    throw new Error(`${source} must be {"rules": [...]} or an array of rules`)
  }
  const seen = new Set<string>()
  const rules = list.map((item, index): RuleSpec => {
    const where = `${source} rule ${index + 1}`
    if (!isObject(item)) throw new Error(`${where} must be an object`)
    const name = typeof item.name === 'string' ? item.name.trim() : ''
    if (!name) throw new Error(`${where} needs a "name": apply matches rules by name`)
    const key = name.toLowerCase()
    if (seen.has(key)) throw new Error(`${where}: the name "${name}" is used twice`)
    seen.add(key)
    if (!isObject(item.expression)) throw new Error(`${where} ("${name}") needs an "expression" object`)
    if (!isObject(item.action)) throw new Error(`${where} ("${name}") needs an "action" object`)
    if (item.mode !== undefined && !MODES.has(item.mode as string)) {
      throw new Error(`${where} ("${name}"): mode must be "enforce" or "log_only"`)
    }
    if (item.enabled !== undefined && typeof item.enabled !== 'boolean') {
      throw new Error(`${where} ("${name}"): enabled must be true or false`)
    }
    const env = item.environment_id
    if (env !== undefined && env !== null && (typeof env !== 'number' || !Number.isInteger(env) || env <= 0)) {
      throw new Error(`${where} ("${name}"): environment_id must be a positive integer or null`)
    }
    return {
      name,
      enabled: item.enabled as boolean | undefined,
      mode: item.mode as RuleSpec['mode'],
      environment_id: env as number | null | undefined,
      expression: item.expression,
      action: item.action,
    }
  })
  return { rules }
}

/** Values the server fills in when a file leaves them out. */
function isDefault(key: string, value: unknown): boolean {
  return (
    (key === 'case_insensitive' && value === false) ||
    (key === 'key' && value === null) ||
    (key === 'values' && Array.isArray(value) && value.length === 0) ||
    (key === 'body' && value === '')
  )
}

/**
 * The comparable form of an expression or action: without editor-only keys
 * (`id` on groups and conditions) and without values equal to the server's
 * defaults, with keys sorted.
 */
export function canonical(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonical)
  if (isObject(value)) {
    const out: Record<string, unknown> = {}
    for (const key of Object.keys(value).sort()) {
      if (key === 'id' || value[key] === undefined || isDefault(key, value[key])) continue
      out[key] = canonical(value[key])
    }
    return out
  }
  return value
}

function same(a: unknown, b: unknown): boolean {
  return JSON.stringify(canonical(a)) === JSON.stringify(canonical(b))
}

/** The rules file form of a saved rule. */
export function toSpec(rule: SavedRule): RuleSpec {
  return {
    name: rule.name,
    enabled: rule.enabled,
    mode: rule.mode,
    environment_id: rule.environment_id ?? null,
    expression: canonical(rule.expression),
    action: canonical(rule.action),
  }
}

/**
 * Plans making the project's rules equal to `desired`: rules are matched by
 * name (case-insensitive), changed ones are updated in place so their ids and
 * counters survive, missing ones are created, and rules absent from the file
 * are deleted.
 */
export function planApply(current: SavedRule[], desired: RuleSpec[]): ApplyPlan {
  const byName = new Map(current.map((r) => [r.name.trim().toLowerCase(), r]))
  const plan: ApplyPlan = { create: [], update: [], remove: [], unchanged: [], order: [] }
  const kept = new Set<number>()
  for (const spec of desired) {
    plan.order.push(spec.name)
    const existing = byName.get(spec.name.toLowerCase())
    if (!existing) {
      plan.create.push(spec)
      continue
    }
    kept.add(existing.id)
    const changed =
      (spec.enabled ?? true) !== existing.enabled ||
      (spec.mode ?? 'enforce') !== existing.mode ||
      (spec.environment_id ?? null) !== (existing.environment_id ?? null) ||
      spec.name !== existing.name ||
      !same(spec.expression, existing.expression) ||
      !same(spec.action, existing.action)
    if (changed) {
      plan.update.push({ id: existing.id, revision: existing.revision, name: existing.name, spec })
    } else {
      plan.unchanged.push(spec.name)
    }
  }
  for (const rule of current) {
    if (!kept.has(rule.id)) plan.remove.push({ id: rule.id, name: rule.name })
  }
  return plan
}

export function planIsEmpty(plan: ApplyPlan, currentOrder: string[]): boolean {
  return (
    plan.create.length === 0 &&
    plan.update.length === 0 &&
    plan.remove.length === 0 &&
    plan.order.map((n) => n.toLowerCase()).join('\n') === currentOrder.map((n) => n.toLowerCase()).join('\n')
  )
}

/** `Name: value` lines into the API's header list. */
export function parseHeaders(lines: string[]): Array<{ name: string; value: string }> {
  return lines.map((line) => {
    const colon = line.indexOf(':')
    if (colon <= 0) throw new Error(`--header must be "Name: value", got "${line}"`)
    return { name: line.slice(0, colon).trim(), value: line.slice(colon + 1).trim() }
  })
}

/** Parses a JSON flag value, naming the flag when it is not JSON. */
export function parseJsonFlag(raw: string, flag: string): unknown {
  try {
    return JSON.parse(raw)
  } catch (e) {
    throw new Error(`${flag} must be JSON: ${(e as Error).message}`)
  }
}

/** One-line description of the engine's decision. */
export function describeDecision(decision: { kind: string; status?: number; location?: string; rule_id?: number }): string {
  switch (decision.kind) {
    case 'forward':
      return 'forwarded to the app (no rule decided)'
    case 'block':
      return `blocked with ${decision.status} by rule ${decision.rule_id}`
    case 'allow':
      return `allowed by rule ${decision.rule_id} (later edge rules skipped)`
    case 'challenge':
      return `challenged by rule ${decision.rule_id}`
    case 'redirect':
      return `redirected (${decision.status}) to ${decision.location} by rule ${decision.rule_id}`
    default:
      return decision.kind
  }
}
