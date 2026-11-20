// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Flag evaluation. Pure, synchronous, total — no I/O, never throws.
 *
 * A deliberate mirror of `evaluate()` in `crates/temps-flags/src/eval.rs`. If
 * you change the resolution order or a rule semantic here, change it there in
 * the same commit, or the same flag resolves differently in the app and on
 * the server. `evaluate.test.ts` pins the shared bucketing vectors.
 *
 * Resolution order:
 *
 *   1. flag missing or archived   -> caller's fallback   FLAG_NOT_FOUND
 *   2. enabled === false          -> default_value       DISABLED
 *   3. first matching rule        -> its serve           RULE_MATCH{i}
 *                                                      | PERCENTAGE_ROLLOUT{i}
 *   4. environment value present  -> that value          ENVIRONMENT_VALUE
 *   5. otherwise                  -> default_value       DEFAULT
 *
 * Rule semantics (same as the server):
 * - A condition on an attribute the context lacks is false, except
 *   `not_exists`. Targeting fails closed.
 * - A rule needing a bucketing subject (rollout < 100, or a split) is skipped
 *   when the context has no `key`.
 * - A rule that cannot be evaluated (undeclared variant, bad pattern, split
 *   not summing to 100) makes the flag serve its default with reason `ERROR`.
 */

import { sha256Prefix32 } from './sha256';
import type {
  AttributeValue,
  EvalContext,
  EvalReason,
  Evaluation,
  FlagRule,
  FlagSnapshot,
  FlagValueType,
  FlagVariant,
  RuleCondition,
  VariantWeight,
} from './types';

/**
 * Longest string a `matches` condition is tested against. JavaScript regular
 * expressions backtrack (the server's do not), so the SDK bounds the input a
 * pattern can be run on. Longer values never match.
 */
export const MAX_MATCHED_LENGTH = 1024;

/**
 * Whether a concrete JSON value is admissible for a declared flag type.
 *
 * `json` accepts anything except `null`: a flag must always resolve to
 * something usable, and `null` is indistinguishable from "unset".
 */
export function valueMatchesType(valueType: FlagValueType, value: unknown): boolean {
  switch (valueType) {
    case 'bool':
      return typeof value === 'boolean';
    case 'string':
      return typeof value === 'string';
    case 'number':
      return typeof value === 'number' && Number.isFinite(value);
    case 'json':
      return value !== null && value !== undefined;
    default:
      return false;
  }
}

/**
 * Stable bucket in [0, 100]. Identical to `bucket()` on the server: the first
 * 32 bits of SHA-256 over `key:salt:subject`.
 */
export function bucket(flagKey: string, salt: string, subject: string): number {
  return (sha256Prefix32(`${flagKey}:${salt}:${subject}`) / 0xffffffff) * 100;
}

/** The split position, independent of the rollout bucket. */
export function splitBucket(flagKey: string, salt: string, subject: string): number {
  return bucket(`${flagKey}#split`, salt, subject);
}

function outcome<T>(value: unknown, reason: EvalReason, variant: string | null = null): Evaluation<T> {
  return { value: value as T, variant, reason };
}

function degraded<T>(flag: FlagSnapshot, fallback: T): Evaluation<T> {
  if (valueMatchesType(flag.value_type, flag.default_value)) {
    return outcome<T>(flag.default_value, { kind: 'ERROR' });
  }
  return outcome<T>(fallback, { kind: 'ERROR' });
}

/**
 * Serve `value` if it matches the flag's declared type, otherwise degrade to
 * the flag default. An operator typing the wrong thing into the flag UI must
 * not break the running app.
 */
function sanitized<T>(
  flag: FlagSnapshot,
  value: unknown,
  reason: EvalReason,
  fallback: T,
  variant: string | null = null,
): Evaluation<T> {
  if (valueMatchesType(flag.value_type, value)) {
    return outcome<T>(value, reason, variant);
  }
  return degraded(flag, fallback);
}

class Malformed extends Error {}

function asNumber(value: AttributeValue): number | null {
  if (typeof value === 'number') return Number.isFinite(value) ? value : null;
  if (typeof value === 'string') {
    const trimmed = value.trim();
    if (trimmed === '') return null;
    const parsed = Number(trimmed);
    return Number.isFinite(parsed) ? parsed : null;
  }
  return null;
}

function looselyEquals(a: AttributeValue, b: AttributeValue): boolean {
  if (typeof a === 'boolean' || typeof b === 'boolean') return a === b;
  if (typeof a === typeof b) return a === b;
  const x = asNumber(a);
  const y = asNumber(b);
  return x !== null && y !== null && x === y;
}

const patternCache = new Map<string, RegExp | null>();
const PATTERN_CACHE_LIMIT = 256;

function compiledPattern(source: string): RegExp {
  let compiled = patternCache.get(source);
  if (compiled === undefined) {
    try {
      compiled = source.length > 256 ? null : new RegExp(`^(?:${source})$`, 'u');
    } catch {
      compiled = null;
    }
    if (patternCache.size >= PATTERN_CACHE_LIMIT) patternCache.clear();
    patternCache.set(source, compiled);
  }
  if (compiled === null) throw new Malformed();
  return compiled;
}

function single(condition: RuleCondition): AttributeValue {
  const value = condition.value;
  if (value === undefined || value === null || Array.isArray(value)) throw new Malformed();
  return value;
}

function list(condition: RuleCondition): AttributeValue[] {
  if (!Array.isArray(condition.value)) throw new Malformed();
  return condition.value;
}

function conditionHolds(condition: RuleCondition, attributes: Record<string, AttributeValue>): boolean {
  const has = Object.prototype.hasOwnProperty.call(attributes, condition.attribute);
  const actual = has ? attributes[condition.attribute] : undefined;
  if (actual === undefined || actual === null) {
    return condition.operator === 'not_exists';
  }
  const text = typeof actual === 'string' ? actual : null;
  switch (condition.operator) {
    case 'exists':
      return true;
    case 'not_exists':
      return false;
    case 'eq':
      return looselyEquals(actual, single(condition));
    case 'neq':
      return !looselyEquals(actual, single(condition));
    case 'in':
      return list(condition).some((v) => looselyEquals(actual, v));
    case 'not_in':
      return !list(condition).some((v) => looselyEquals(actual, v));
    case 'contains':
    case 'not_contains': {
      const needle = single(condition);
      if (typeof needle !== 'string') throw new Malformed();
      if (text === null) return false;
      return condition.operator === 'contains' ? text.includes(needle) : !text.includes(needle);
    }
    case 'matches': {
      const source = single(condition);
      if (typeof source !== 'string') throw new Malformed();
      const pattern = compiledPattern(source);
      return text !== null && text.length <= MAX_MATCHED_LENGTH && pattern.test(text);
    }
    case 'gt':
    case 'gte':
    case 'lt':
    case 'lte': {
      const operand = asNumber(single(condition));
      if (operand === null) throw new Malformed();
      const value = asNumber(actual);
      if (value === null) return false;
      if (condition.operator === 'gt') return value > operand;
      if (condition.operator === 'gte') return value >= operand;
      if (condition.operator === 'lt') return value < operand;
      return value <= operand;
    }
    default:
      throw new Malformed();
  }
}

function findVariant(flag: FlagSnapshot, key: string): FlagVariant {
  const variant = (flag.variants ?? []).find((v) => v.key === key);
  if (!variant) throw new Malformed();
  return variant;
}

/**
 * A split is evaluable when it is non-empty, its weights sum to exactly 100 and
 * every arm names a declared variant. Checked before the subject, as the Rust
 * evaluator does, so a broken split is an `ERROR` whoever is asking.
 */
function checkSplit(flag: FlagSnapshot, split: VariantWeight[]): void {
  const total = split.reduce((sum, arm) => sum + arm.weight, 0);
  if (split.length === 0 || total !== 100) throw new Malformed();
  for (const arm of split) findVariant(flag, arm.variant);
}

/** Called after `checkSplit`. */
function pickSplit(flag: FlagSnapshot, split: VariantWeight[], subject: string): string {
  const position = splitBucket(flag.key, flag.salt ?? '', subject);
  let cumulative = 0;
  for (const arm of split) {
    cumulative += arm.weight;
    if (position < cumulative) return arm.variant;
  }
  const last = [...split].reverse().find((arm) => arm.weight > 0);
  if (!last) throw new Malformed();
  return last.variant;
}

/** The rule's outcome, `null` when it does not match. Throws `Malformed`. */
function evaluateRule<T>(
  flag: FlagSnapshot,
  rule: FlagRule,
  index: number,
  context: EvalContext,
  fallback: T,
): Evaluation<T> | null {
  const attributes = context.attributes ?? {};
  for (const condition of rule.conditions ?? []) {
    if (!conditionHolds(condition, attributes)) return null;
  }

  const subject = context.key ? context.key : null;
  let bucketed = false;
  const percentage = rule.rollout_percentage;
  if (percentage !== undefined && percentage !== null) {
    if (!Number.isFinite(percentage) || percentage < 0 || percentage > 100) throw new Malformed();
    if (percentage < 100) {
      if (subject === null) return null;
      if (bucket(flag.key, flag.salt ?? '', subject) >= percentage) return null;
      bucketed = true;
    }
  }
  const reason = (byBucket: boolean): EvalReason =>
    byBucket ? { kind: 'PERCENTAGE_ROLLOUT', index } : { kind: 'RULE_MATCH', index };

  const serve = rule.serve;
  switch (serve?.kind) {
    case 'value':
      return sanitized<T>(flag, serve.value, reason(bucketed), fallback);
    case 'variant': {
      const variant = findVariant(flag, serve.variant);
      return sanitized<T>(flag, variant.value, reason(bucketed), fallback, variant.key);
    }
    case 'split': {
      checkSplit(flag, serve.split ?? []);
      if (subject === null) return null;
      const variant = findVariant(flag, pickSplit(flag, serve.split ?? [], subject));
      return sanitized<T>(flag, variant.value, reason(true), fallback, variant.key);
    }
    default:
      throw new Malformed();
  }
}

/** Evaluate one flag. Always returns a usable value. */
export function evaluate<T>(
  flag: FlagSnapshot | undefined,
  context: EvalContext,
  fallback: T,
): Evaluation<T> {
  if (!flag) {
    return outcome<T>(fallback, { kind: 'FLAG_NOT_FOUND' });
  }

  if (!flag.enabled) {
    return sanitized<T>(flag, flag.default_value, { kind: 'DISABLED' }, fallback);
  }

  const rules = flag.rules ?? [];
  for (let index = 0; index < rules.length; index++) {
    try {
      const result = evaluateRule<T>(flag, rules[index], index, context ?? {}, fallback);
      if (result) return result;
    } catch (error) {
      if (error instanceof Malformed) return degraded(flag, fallback);
      throw error;
    }
  }

  if (flag.environment_value !== null && flag.environment_value !== undefined) {
    return sanitized<T>(flag, flag.environment_value, { kind: 'ENVIRONMENT_VALUE' }, fallback);
  }

  return sanitized<T>(flag, flag.default_value, { kind: 'DEFAULT' }, fallback);
}
