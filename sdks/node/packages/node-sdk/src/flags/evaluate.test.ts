// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'vitest';
import { createHash } from 'node:crypto';
import { bucket, evaluate, splitBucket, valueMatchesType } from './evaluate';
import { sha256 } from './sha256';
import type { FlagRule, FlagSnapshot } from './types';

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

  // Phase 1 pinned "context is ignored". With targeting shipped, what still
  // holds is that a flag without rules ignores context, so existing flags
  // serve exactly what they did before the upgrade.
  it('ignores context for a flag without rules', () => {
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

function variantFlag(rules: FlagRule[]): FlagSnapshot {
  return {
    key: 'checkout.v2',
    value_type: 'string',
    default_value: 'off',
    enabled: true,
    environment_value: null,
    salt: 'a1b2c3',
    variants: [
      { key: 'control', value: 'old' },
      { key: 'test', value: 'new' },
    ],
    rules,
  };
}

const fiftyFifty: FlagRule = {
  serve: {
    kind: 'split',
    split: [
      { variant: 'control', weight: 50 },
      { variant: 'test', weight: 50 },
    ],
  },
};

describe('bucketing', () => {
  it('computes SHA-256 exactly like node:crypto', () => {
    for (const input of ['', 'abc', 'checkout.v2:a1b2c3:user_1001', 'é'.repeat(70), 'x'.repeat(200)]) {
      const expected = createHash('sha256').update(input, 'utf8').digest('hex');
      expect(Buffer.from(sha256(input)).toString('hex')).toBe(expected);
    }
  });

  // The server's locked vectors (crates/temps-flags/src/eval.rs). If these
  // drift, the SDK assigns users to different cohorts than the server.
  it.each([
    ['checkout.v2', 'a1b2c3', 'user_1001', 36.47],
    ['checkout.v2', 'a1b2c3', 'user_1002', 21.03],
    ['checkout.v2', 'a1b2c3', 'user_1003', 92.11],
    ['checkout.v2', 'z9y8x7', 'user_1003', 8.39],
    ['new.search', 'a1b2c3', 'user_1001', 0.33],
  ])('bucket(%s, %s, %s) = %d', (key, salt, subject, expected) => {
    expect(Math.abs(bucket(key, salt, subject) - expected)).toBeLessThan(0.005);
  });

  it.each([
    ['checkout.v2', 'a1b2c3', 'user_1001', 94.69],
    ['checkout.v2', 'a1b2c3', 'user_1002', 14.38],
    ['checkout.v2', 'z9y8x7', 'user_1001', 77.71],
  ])('splitBucket(%s, %s, %s) = %d', (key, salt, subject, expected) => {
    expect(Math.abs(splitBucket(key, salt, subject) - expected)).toBeLessThan(0.005);
  });
});

describe('targeting rules', () => {
  it('serves a matching rule ahead of the environment value', () => {
    const flag: FlagSnapshot = {
      ...snapshot(true, false),
      rules: [{ conditions: [{ attribute: 'plan', operator: 'eq', value: 'pro' }], serve: { kind: 'value', value: true } }],
    };
    expect(evaluate(flag, { attributes: { plan: 'pro' } }, false)).toEqual({
      value: true,
      variant: null,
      reason: { kind: 'RULE_MATCH', index: 0 },
    });
    expect(evaluate(flag, { attributes: { plan: 'free' } }, false).reason).toEqual({ kind: 'ENVIRONMENT_VALUE' });
  });

  it('lets the kill switch beat every rule', () => {
    const flag: FlagSnapshot = { ...snapshot(false, null), rules: [{ serve: { kind: 'value', value: true } }] };
    expect(evaluate(flag, { key: 'u' }, true).reason).toEqual({ kind: 'DISABLED' });
  });

  it('fails closed on missing attributes, except not_exists', () => {
    const operators = ['eq', 'neq', 'in', 'not_in', 'contains', 'not_contains', 'gt', 'exists'] as const;
    for (const operator of operators) {
      const value = operator === 'exists' ? undefined : operator.endsWith('in') ? ['x'] : operator === 'gt' ? 1 : 'x';
      const flag: FlagSnapshot = {
        ...snapshot(true, null),
        rules: [{ conditions: [{ attribute: 'plan', operator, value }], serve: { kind: 'value', value: true } }],
      };
      expect(evaluate(flag, {}, false).value).toBe(false);
    }
    const notExists: FlagSnapshot = {
      ...snapshot(true, null),
      rules: [{ conditions: [{ attribute: 'plan', operator: 'not_exists' }], serve: { kind: 'value', value: true } }],
    };
    expect(evaluate(notExists, {}, false).value).toBe(true);
  });

  it('compares like the server', () => {
    const holds = (condition: FlagRule['conditions'], attributes: Record<string, string | number | boolean>) =>
      evaluate({ ...snapshot(true, null), rules: [{ conditions: condition, serve: { kind: 'value', value: true } }] }, { attributes }, false)
        .value === true;
    expect(holds([{ attribute: 'a', operator: 'eq', value: 42 }], { a: '42' })).toBe(true);
    expect(holds([{ attribute: 'a', operator: 'eq', value: 'true' }], { a: true })).toBe(false);
    expect(holds([{ attribute: 'a', operator: 'matches', value: '[a-z]+@acme\\.test' }], { a: 'jo@acme.test' })).toBe(true);
    expect(holds([{ attribute: 'a', operator: 'matches', value: 'acme' }], { a: 'jo@acme.test' })).toBe(false);
    expect(holds([{ attribute: 'a', operator: 'gte', value: 10 }], { a: '10' })).toBe(true);
    expect(holds([{ attribute: 'a', operator: 'lt', value: 10 }], { a: 'ten' })).toBe(false);
    expect(holds([{ attribute: 'a', operator: 'contains', value: '@acme' }], { a: 'jo@acme.test' })).toBe(true);
  });

  it('includes exactly the subjects whose bucket is below the rollout', () => {
    const flag: FlagSnapshot = {
      ...snapshot(true, null),
      salt: 'a1b2c3',
      rules: [{ rollout_percentage: 25, serve: { kind: 'value', value: true } }],
    };
    for (let i = 0; i < 500; i++) {
      const subject = `user_${i}`;
      expect(evaluate(flag, { key: subject }, false).value).toBe(bucket('checkout.v2', 'a1b2c3', subject) < 25);
    }
    expect(evaluate(flag, {}, false).reason).toEqual({ kind: 'DEFAULT' });
  });

  it('splits by the independent split bucket and names the variant', () => {
    const flag = variantFlag([fiftyFifty]);
    const result = evaluate(flag, { key: 'user_1001' }, 'fallback');
    // splitBucket(user_1001) = 94.69, past control's 50.
    expect(result).toEqual({ value: 'new', variant: 'test', reason: { kind: 'PERCENTAGE_ROLLOUT', index: 0 } });
    expect(evaluate(flag, { key: 'user_1002' }, 'fallback').variant).toBe('control');
  });

  it('degrades malformed rules to the default with ERROR', () => {
    const undeclared = variantFlag([{ serve: { kind: 'variant', variant: 'ghost' } }]);
    expect(evaluate(undeclared, {}, 'fallback')).toEqual({ value: 'off', variant: null, reason: { kind: 'ERROR' } });

    const badSplit = variantFlag([{ serve: { kind: 'split', split: [] } }]);
    expect(evaluate(badSplit, { key: 'u' }, 'fallback').reason).toEqual({ kind: 'ERROR' });
    // Without a subject too: whether a broken split is noticed must not
    // depend on who is asking (mirrors the Rust evaluator).
    expect(evaluate(badSplit, {}, 'fallback').reason).toEqual({ kind: 'ERROR' });
    const ghostArm = variantFlag([
      { serve: { kind: 'split', split: [{ variant: 'control', weight: 50 }, { variant: 'ghost', weight: 50 }] } },
    ]);
    expect(evaluate(ghostArm, {}, 'fallback').reason).toEqual({ kind: 'ERROR' });

    const badPattern = variantFlag([
      { conditions: [{ attribute: 'a', operator: 'matches', value: '(open' }], serve: { kind: 'value', value: 'x' } },
    ]);
    expect(evaluate(badPattern, { attributes: { a: 'x' } }, 'fallback').reason).toEqual({ kind: 'ERROR' });

    // Balances only against the anchoring wrapper; the server refuses it too.
    const escapingPattern = variantFlag([
      { conditions: [{ attribute: 'a', operator: 'matches', value: 'pro)|(?:team' }], serve: { kind: 'value', value: 'x' } },
    ]);
    expect(evaluate(escapingPattern, { attributes: { a: 'professional' } }, 'fallback').reason).toEqual({ kind: 'ERROR' });
  });

  it('reports a malformed split even behind a partial rollout', () => {
    const flag = variantFlag([
      {
        rollout_percentage: 10,
        serve: { kind: 'split', split: [{ variant: 'control', weight: 50 }, { variant: 'test', weight: 40 }] },
      },
    ]);
    expect(evaluate(flag, {}, 'fallback').reason).toEqual({ kind: 'ERROR' });
    for (const key of ['a', 'b', 'c', 'd', 'e']) {
      expect(evaluate(flag, { key }, 'fallback').reason).toEqual({ kind: 'ERROR' });
    }
  });

  it('reads only decimal numbers from strings, like the server', () => {
    const gte = (attribute: string) =>
      evaluate(
        { ...snapshot(true, null), rules: [{ conditions: [{ attribute: 'a', operator: 'gte', value: 10 }], serve: { kind: 'value', value: true } }] },
        { attributes: { a: attribute } },
        false,
      ).value === true;
    expect(gte(' 16 ')).toBe(true);
    expect(gte('1e2')).toBe(true);
    expect(gte('+12.5')).toBe(true);
    expect(gte('0x10')).toBe(false);
    expect(gte('0b1111')).toBe(false);
    expect(gte('Infinity')).toBe(false);
  });
});
