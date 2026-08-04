/**
 * Feature-flag types. These mirror `crates/temps-flags/src/eval.rs` exactly —
 * the SDK evaluates locally, so the two implementations must agree or the same
 * flag resolves differently on the server and in the app.
 */

export type FlagValueType = 'bool' | 'string' | 'number' | 'json';

/**
 * Why an evaluation produced the value it did.
 *
 * `RULE_MATCH` and `PERCENTAGE_ROLLOUT` are declared because they are part of
 * the wire contract, but the server never emits them yet — targeting has not
 * shipped. Code that switches on `reason` should handle them from day one.
 */
export type EvalReason =
  | { kind: 'FLAG_NOT_FOUND' }
  | { kind: 'DISABLED' }
  | { kind: 'RULE_MATCH'; index: number }
  | { kind: 'PERCENTAGE_ROLLOUT'; index: number }
  | { kind: 'ENVIRONMENT_VALUE' }
  | { kind: 'DEFAULT' }
  | { kind: 'ERROR' };

/** One flag, already resolved down to a single environment. */
export interface FlagSnapshot {
  key: string;
  value_type: FlagValueType;
  default_value: unknown;
  /** `false` means the kill switch is engaged for this environment. */
  enabled: boolean;
  /** `null` means "inherit `default_value`". */
  environment_value?: unknown | null;
}

export interface FlagSnapshotResponse {
  environment_id: number;
  flags: FlagSnapshot[];
}

/**
 * Everything you know about the subject being evaluated.
 *
 * Accepted and ignored today: targeting has not shipped. It is in the signature
 * now so that when it does, you add attributes to a call you have already
 * written instead of editing every call site.
 */
export interface EvalContext {
  /** Stable bucketing subject (user id, account id, device id). */
  key?: string;
  /** Targeting attributes, e.g. `{ plan: 'enterprise', region: 'eu' }`. */
  attributes?: Record<string, string | number | boolean>;
}

export interface Evaluation<T> {
  value: T;
  /** Named variant, once variants exist. Always `null` today. */
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
