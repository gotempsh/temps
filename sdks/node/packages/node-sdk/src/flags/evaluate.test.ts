// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'vitest';
import { evaluate, valueMatchesType } from './evaluate';
import type { FlagSnapshot } from './types';

/**
 * These mirror the Rust tests in `crates/temps-flags/src/eval.rs`. The SDK
 * evaluates locally, so any divergence means the same flag resolves one way in
 * the app and another on the server.
 */

function snapshot(enabled: boolean, environmentValue: unknown | null): FlagSnapshot {
  return {
    key: 'checkout.v2',
    value_type: 'bool',
    default_value: false,
    enabled,
    environment_value: environmentValue,
  };
}

describe('evaluate', () => {
  it('serves the environment value when set', () => {
    const result = evaluate(snapshot(true, true), {}, false);

    expect(result.value).toBe(true);
    expect(result.reason).toEqual({ kind: 'ENVIRONMENT_VALUE' });
    expect(result.variant).toBeNull();
  });

  it('falls back to the flag default when no environment value is set', () => {
    const result = evaluate(snapshot(true, null), {}, true);

    expect(result.value).toBe(false);
    expect(result.reason).toEqual({ kind: 'DEFAULT' });
  });

  it('returns the caller fallback for an unknown flag', () => {
    const result = evaluate(undefined, {}, 'my-fallback');

    expect(result.value).toBe('my-fallback');
    expect(result.reason).toEqual({ kind: 'FLAG_NOT_FOUND' });
  });

  // The kill switch must outrank the environment value, or "disable this flag"
  // would not disable a flag that has an override set.
  it('lets the kill switch beat the environment value', () => {
    const result = evaluate(snapshot(false, true), {}, true);

    expect(result.value).toBe(false);
    expect(result.reason).toEqual({ kind: 'DISABLED' });
  });

  // Asserted explicitly so that the day targeting lands, this test fails and
  // forces a conscious update rather than silently changing behaviour.
  it('ignores context in phase 1', () => {
    const flag = snapshot(true, true);

    const withContext = evaluate(flag, { key: 'user_1001', attributes: { plan: 'free' } }, false);
    const withoutContext = evaluate(flag, {}, false);

    expect(withContext).toEqual(withoutContext);
  });

  it('degrades a type-mismatched value to the flag default', () => {
    const result = evaluate(snapshot(true, 'yes'), {}, true);

    expect(result.value).toBe(false);
    expect(result.reason).toEqual({ kind: 'ERROR' });
  });

  it('is total even when the default is also invalid', () => {
    const flag: FlagSnapshot = {
      key: 'broken',
      value_type: 'number',
      default_value: 'not a number',
      enabled: true,
      environment_value: 'also not a number',
    };

    const result = evaluate(flag, {}, 42);

    expect(result.value).toBe(42);
    expect(result.reason).toEqual({ kind: 'ERROR' });
  });

  it('supports non-boolean flag types', () => {
    const flag: FlagSnapshot = {
      key: 'worker.batch_size',
      value_type: 'number',
      default_value: 50,
      enabled: true,
      environment_value: 200,
    };

    expect(evaluate(flag, {}, 0).value).toBe(200);
  });

  it('treats an undefined environment value the same as null', () => {
    const flag: FlagSnapshot = {
      key: 'checkout.v2',
      value_type: 'bool',
      default_value: false,
      enabled: true,
    };

    expect(evaluate(flag, {}, true).reason).toEqual({ kind: 'DEFAULT' });
  });
});

describe('valueMatchesType', () => {
  it('matches declared types', () => {
    expect(valueMatchesType('bool', true)).toBe(true);
    expect(valueMatchesType('bool', 1)).toBe(false);
    expect(valueMatchesType('number', 1.5)).toBe(true);
    expect(valueMatchesType('number', '1.5')).toBe(false);
    expect(valueMatchesType('string', 'x')).toBe(true);
    expect(valueMatchesType('json', { a: 1 })).toBe(true);
  });

  it('never accepts null, which is indistinguishable from unset', () => {
    expect(valueMatchesType('json', null)).toBe(false);
    expect(valueMatchesType('bool', null)).toBe(false);
  });

  it('rejects NaN and Infinity, which do not survive JSON', () => {
    expect(valueMatchesType('number', NaN)).toBe(false);
    expect(valueMatchesType('number', Infinity)).toBe(false);
  });
});
