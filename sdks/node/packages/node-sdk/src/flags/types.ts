// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Feature-flag types. These mirror `crates/temps-flags/src/eval.rs` exactly —
 * the SDK evaluates locally, so the two implementations must agree or the same
 * flag resolves differently on the server and in the app.
 */

export type FlagValueType = 'bool' | 'string' | 'number' | 'json';

/**
 * Why an evaluation produced the value it did.
 *
 * `RULE_MATCH` means targeting rule `index` matched on its conditions;
 * `PERCENTAGE_ROLLOUT` means the subject's bucket decided it (a rollout
 * percentage or a variant split).
 */
export type EvalReason =
  | { kind: 'FLAG_NOT_FOUND' }
  | { kind: 'DISABLED' }
  | { kind: 'RULE_MATCH'; index: number }
  | { kind: 'PERCENTAGE_ROLLOUT'; index: number }
  | { kind: 'ENVIRONMENT_VALUE' }
  | { kind: 'DEFAULT' }
  | { kind: 'ERROR' };

export type AttributeValue = string | number | boolean;

/** A named value a flag can serve. */
export interface FlagVariant {
  key: string;
  value: unknown;
  description?: string | null;
}

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
  | 'not_exists';

export interface RuleCondition {
  attribute: string;
  operator: ConditionOperator;
  /** Absent for `exists`/`not_exists`; a list for `in`/`not_in`. */
  value?: AttributeValue | AttributeValue[] | null;
}

export interface VariantWeight {
  variant: string;
  weight: number;
}

export type RuleServe =
  | { kind: 'value'; value: unknown }
  | { kind: 'variant'; variant: string }
  | { kind: 'split'; split: VariantWeight[] };

/** `when <all conditions> [within rollout_percentage] serve <serve>`. */
export interface FlagRule {
  description?: string | null;
  conditions?: RuleCondition[];
  /** 0-100; absent means every matching subject. */
  rollout_percentage?: number | null;
  serve: RuleServe;
}

/** One flag, already resolved down to a single environment. */
export interface FlagSnapshot {
  key: string;
  value_type: FlagValueType;
  default_value: unknown;
  /** `false` means the kill switch is engaged for this environment. */
  enabled: boolean;
  /** `null` means "inherit `default_value`". */
  environment_value?: unknown | null;
  /** Bucketing salt for rollouts and splits. Server-side only. */
  salt?: string;
  variants?: FlagVariant[];
  /** Ordered targeting rules; first match wins. */
  rules?: FlagRule[];
}

export interface FlagSnapshotResponse {
  environment_id: number;
  flags: FlagSnapshot[];
}

/** Everything you know about the subject being evaluated. */
export interface EvalContext {
  /**
   * Stable bucketing subject (user id, account id, device id). Rules with a
   * rollout percentage or a variant split are skipped without one.
   */
  key?: string;
  /** Targeting attributes, e.g. `{ plan: 'enterprise', country: 'DE' }`. */
  attributes?: Record<string, AttributeValue>;
}

export interface Evaluation<T> {
  value: T;
  /** The variant a targeting rule served, or `null` for plain values. */
  variant: string | null;
  reason: EvalReason;
}

export interface FlagsClientOptions {
  /**
   * Control-plane API base URL. Defaults to `TEMPS_API_URL`, which Temps
   * injects into every deployment.
   */
  apiUrl?: string;
  /**
   * Deployment token. Defaults to `TEMPS_API_TOKEN`, injected alongside
   * `TEMPS_API_URL`.
   */
  apiToken?: string;
  /**
   * Only needed when the token is project-wide rather than scoped to one
   * environment.
   */
  environmentId?: number;
  /**
   * Background refresh interval in milliseconds. Default 30s. Unchanged flags
   * cost a 304, not a payload.
   *
   * Falls back to `TEMPS_FLAGS_REFRESH_INTERVAL_MS` when unset, so a
   * deployment can tighten (or loosen) polling without a code change or
   * redeploy of the SDK itself — just the env var.
   */
  refreshIntervalMs?: number;
  /** Per-request timeout in milliseconds. Default 5s. */
  timeoutMs?: number;
  /**
   * Report which flag keys were actually evaluated, on the refresh interval.
   * Defaults to `true`.
   *
   * This is what makes "last evaluated" in the console meaningful — the server
   * hands out the whole flag set and evaluation happens locally, so without it
   * the control plane cannot tell a flag live code still reads from one nothing
   * has called in a year. Only keys that exist in the snapshot are reported,
   * and only on the interval, never per call.
   */
  reportExposure?: boolean;
  /**
   * Called when a background refresh fails. Defaults to a `console.warn`.
   * Failures never throw and never clear the cache — the last good snapshot
   * keeps serving.
   */
  onError?: (error: Error) => void;
}
