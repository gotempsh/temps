// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Flag evaluation. Pure, synchronous, total — no I/O, never throws.
 *
 * This is a deliberate line-for-line mirror of `evaluate()` in
 * `crates/temps-flags/src/eval.rs`. If you change the resolution order here,
 * change it there in the same commit, or the same flag will resolve
 * differently in the app and on the server.
 *
 * Resolution order:
 *
 *   1. flag missing or archived   -> caller's fallback   FLAG_NOT_FOUND
 *   2. enabled === false          -> default_value       DISABLED
 *   3. -- reserved for rules --                          RULE_MATCH{i}
 *                                                      | PERCENTAGE_ROLLOUT{i}
 *   4. environment value present  -> that value          ENVIRONMENT_VALUE
 *   5. otherwise                  -> default_value       DEFAULT
 */

import type { EvalContext, Evaluation, FlagSnapshot, FlagValueType } from './types';

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

function outcome<T>(value: unknown, reason: Evaluation<T>['reason']): Evaluation<T> {
  return { value: value as T, variant: null, reason };
}

/**
 * Serve `value` if it matches the flag's declared type, otherwise degrade to
 * the flag default. An operator typing the wrong thing into the flag UI must
 * not break the running app.
 */
function sanitized<T>(
  flag: FlagSnapshot,
  value: unknown,
  reason: Evaluation<T>['reason'],
  fallback: T,
): Evaluation<T> {
  if (valueMatchesType(flag.value_type, value)) {
    return outcome<T>(value, reason);
  }
  if (valueMatchesType(flag.value_type, flag.default_value)) {
    return outcome<T>(flag.default_value, { kind: 'ERROR' });
  }
  return outcome<T>(fallback, { kind: 'ERROR' });
}

/**
 * Evaluate one flag. Always returns a usable value.
 *
 * `_context` is accepted and ignored: targeting has not shipped. The parameter
 * is the expensive half of the contract; the code that reads it is cheap.
 */
export function evaluate<T>(
  flag: FlagSnapshot | undefined,
  _context: EvalContext,
  fallback: T,
): Evaluation<T> {
  if (!flag) {
    return outcome<T>(fallback, { kind: 'FLAG_NOT_FOUND' });
  }

  if (!flag.enabled) {
    return sanitized<T>(flag, flag.default_value, { kind: 'DISABLED' }, fallback);
  }

  // -------------------------------------------------------------------------
  // Step 3: targeting rules evaluate HERE, ahead of the environment value and
  // behind the kill switch. Nothing above or below this point changes when
  // they land.
  // -------------------------------------------------------------------------

  if (flag.environment_value !== null && flag.environment_value !== undefined) {
    return sanitized<T>(flag, flag.environment_value, { kind: 'ENVIRONMENT_VALUE' }, fallback);
  }

  return sanitized<T>(flag, flag.default_value, { kind: 'DEFAULT' }, fallback);
}
