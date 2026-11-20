// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Feature flags in the browser.
 *
 * Reads `client_visible` flags, already evaluated by the server, from the
 * same-origin `POST {basePath}/flags` endpoint. The browser never sees rules,
 * salts or server-only flags; it receives a value, a variant and a reason per
 * flag, for the visitor making the request.
 *
 * - **Lazy**: nothing is fetched until the app first reads a flag (or calls
 *   `ready()` / `reload()`), so a site that never uses flags pays nothing.
 * - **Synchronous reads**: `get()` returns the fallback until the flags have
 *   arrived, then the served value. Subscribe (`subscribe`, or the framework
 *   hooks) to re-render when they arrive.
 * - **Never throws**: a failed request keeps the last good set (or the
 *   fallbacks) and is reported to `onError`.
 * - **Exposure**: when the server marks a flag for exposure recording (opt-in
 *   per flag, sampled per visitor), the first read of each served variant on a
 *   page reports a `flag_exposure` event through `onExposure`.
 */

import { postJson, REQUEST_TIMEOUT_MS } from "./request";
import type { JsonValue } from "./types";

/** Why a flag has the value it has. Mirrors the server's reason. */
export interface FlagReason {
  kind:
    | "FLAG_NOT_FOUND"
    | "DISABLED"
    | "RULE_MATCH"
    | "PERCENTAGE_ROLLOUT"
    | "ENVIRONMENT_VALUE"
    | "DEFAULT"
    | "ERROR";
  index?: number;
}

/** One flag as the server evaluated it for this visitor. */
export interface FlagEvaluation {
  key: string;
  value: unknown;
  variant: string | null;
  reason: FlagReason;
  /** Whether reads of this flag should be recorded as exposures. */
  record_exposure: boolean;
}

/** Targeting context sent with the request. */
export interface FlagContext {
  /**
   * Bucketing subject, e.g. a signed-in user id. Defaults, on the server, to
   * the Temps visitor cookie.
   */
  key?: string;
  /** Targeting attributes, e.g. `{ plan: "pro" }`. */
  attributes?: Record<string, string | number | boolean>;
}

/** What a `flag_exposure` event carries. */
export interface FlagExposure {
  flag_key: string;
  variant: string;
  reason: string;
}

export interface FeatureFlagsOptions extends FlagContext {
  /** Where the Temps endpoints live. Defaults to `/api/_temps`. */
  basePath?: string;
  /**
   * Values rendered on the server (e.g. from the Node SDK), served until the
   * browser's own request completes, so the page never flashes the wrong
   * variant.
   */
  bootstrap?: Record<string, unknown>;
  /** Called on the first read of each exposure-recorded flag variant. */
  onExposure?: (exposure: FlagExposure) => void;
  /** Called when the flags request fails. Defaults to `console.warn`. */
  onError?: (error: Error) => void;
  /** Never fetch; serve bootstrap values and fallbacks only. */
  disabled?: boolean;
  /** Test seam. Defaults to the global `fetch`. */
  fetch?: typeof fetch;
  /** How long the flags request may take, body included. Defaults to 10 s;
   * past it the request fails like any other (bootstrap values stay). */
  timeoutMs?: number;
}

export type FlagsStatus = "idle" | "loading" | "ready" | "error";

interface FlagsResponseBody {
  flags?: FlagEvaluation[];
}

export class FeatureFlags {
  private readonly basePath: string;
  private readonly onExposure?: (exposure: FlagExposure) => void;
  private readonly onError: (error: Error) => void;
  private readonly fetchImpl?: typeof fetch;
  private readonly timeoutMs: number;
  private disabled: boolean;

  private context: FlagContext;
  private results = new Map<string, FlagEvaluation>();
  private bootstrap: Map<string, unknown>;
  private inflight: Promise<void> | null = null;
  private exposed = new Set<string>();
  private listeners = new Set<() => void>();
  private currentStatus: FlagsStatus = "idle";
  private revision = 0;
  private generation = 0;

  constructor(options: FeatureFlagsOptions = {}) {
    this.basePath = options.basePath ?? "/api/_temps";
    this.context = { key: options.key, attributes: options.attributes };
    this.bootstrap = new Map(Object.entries(options.bootstrap ?? {}));
    this.onExposure = options.onExposure;
    this.onError =
      options.onError ??
      ((error) => {
        // eslint-disable-next-line no-console
        console.warn("[temps-flags] serving fallbacks:", error.message);
      });
    this.fetchImpl = options.fetch;
    this.timeoutMs = options.timeoutMs ?? REQUEST_TIMEOUT_MS;
    this.disabled = options.disabled ?? false;
  }

  /** `idle` until the first read, then `loading`, `ready` or `error`. */
  get status(): FlagsStatus {
    return this.currentStatus;
  }

  /** Changes whenever the served flags change. For `useSyncExternalStore`. */
  get version(): number {
    return this.revision;
  }

  /**
   * The value served for `key`, or `fallback` until flags arrive, when the
   * flag does not exist (or is not client-visible), or when the served value
   * is not of the fallback's type.
   */
  get<T>(key: string, fallback: T): T {
    this.ensureLoaded();
    const evaluation = this.results.get(key);
    if (evaluation && compatible(evaluation.value, fallback)) this.recordExposure(evaluation);
    return this.peek(key, fallback);
  }

  /**
   * What `get` would return, without starting the request or recording an
   * exposure. For render phases that must stay free of side effects; pair it
   * with a `get` after commit.
   */
  peek<T>(key: string, fallback: T): T {
    const evaluation = this.results.get(key);
    if (!evaluation) {
      return this.bootstrap.has(key) ? sameType(this.bootstrap.get(key), fallback) : fallback;
    }
    return sameType(evaluation.value, fallback);
  }

  /** What `getVariant` would return, without side effects. */
  peekVariant(key: string): string | null {
    return this.results.get(key)?.variant ?? null;
  }

  /** The variant served for `key`, or `null`. */
  getVariant(key: string): string | null {
    this.ensureLoaded();
    const evaluation = this.results.get(key);
    if (!evaluation) return null;
    this.recordExposure(evaluation);
    return evaluation.variant;
  }

  /**
   * Replace the evaluation context (unlike `reload`, attributes are not
   * merged). Re-fetches only if flags were already requested, so an unread
   * client stays lazy.
   */
  setContext(context: FlagContext): void {
    this.replaceContext({ key: context.key, attributes: context.attributes });
    if (this.currentStatus !== "idle") void this.load();
  }

  /**
   * Turn fetching off or on. Disabling drops any request in flight; enabling
   * fetches on the next read.
   */
  setDisabled(disabled: boolean): void {
    if (disabled === this.disabled) return;
    this.disabled = disabled;
    if (disabled) {
      // A disabled client serves bootstrap values and fallbacks only, so
      // what an earlier request fetched is dropped too.
      this.results = new Map();
      this.generation += 1;
      this.inflight = null;
      if (this.currentStatus === "loading") this.currentStatus = "idle";
    } else if (this.currentStatus !== "idle") {
      void this.load();
      return;
    }
    this.notify();
  }

  /** The full evaluation for `key`, for debugging. Records no exposure. */
  getEvaluation(key: string): FlagEvaluation | undefined {
    this.ensureLoaded();
    return this.results.get(key);
  }

  /** Every served flag. Records no exposure. */
  all(): FlagEvaluation[] {
    this.ensureLoaded();
    return [...this.results.values()];
  }

  /** Resolves once the first request has finished (successfully or not). */
  ready(): Promise<void> {
    this.ensureLoaded();
    return this.inflight ?? Promise.resolve();
  }

  /**
   * Re-evaluate with a new context, e.g. after sign-in. Merges `attributes`
   * over the previous ones; pass `key` to change the bucketing subject.
   */
  reload(context: FlagContext = {}): Promise<void> {
    this.replaceContext({
      key: context.key ?? this.context.key,
      attributes: { ...this.context.attributes, ...context.attributes },
    });
    return this.load();
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  /**
   * Values evaluated for another subject or other attributes must not be
   * read under the new ones: after a sign-out they would be the previous
   * user's, and would stay if the new request failed. A change therefore
   * drops the results, the bootstrap values (rendered for the old context)
   * and the exposures already recorded; an unchanged context keeps the last
   * good values while it refreshes.
   */
  private replaceContext(next: FlagContext): void {
    const changed = contextKey(next) !== contextKey(this.context);
    this.context = next;
    if (!changed) return;
    this.results = new Map();
    this.bootstrap.clear();
    this.exposed.clear();
    this.notify();
  }

  private ensureLoaded(): void {
    if (this.currentStatus === "idle") void this.load();
  }

  private load(): Promise<void> {
    if (this.disabled) return Promise.resolve();
    const fetchImpl = this.fetchImpl ?? (typeof fetch === "function" ? fetch : undefined);
    if (!fetchImpl) {
      this.currentStatus = "error";
      return Promise.resolve();
    }
    // A newer request supersedes an older one: a reload after sign-in must
    // not be overwritten by the anonymous request still in flight.
    const generation = ++this.generation;
    this.currentStatus = "loading";
    // Subscribers re-render on `version`, so the loading state is visible.
    // Deferred: a read during render can start this load, and notifying a
    // store's subscribers synchronously from inside a render is not allowed.
    void Promise.resolve().then(() => {
      if (generation === this.generation && this.currentStatus === "loading") this.notify();
    });
    const body = JSON.stringify({ key: this.context.key, attributes: this.context.attributes ?? {} });

    const request = (async () => {
      try {
        const parsed = await postJson<FlagsResponseBody>(
          fetchImpl,
          `${this.basePath}/flags`,
          body,
          this.timeoutMs
        );
        if (generation !== this.generation) return;
        this.results = new Map((parsed.flags ?? []).map((flag) => [flag.key, flag]));
        // The response is authoritative: a bootstrapped flag it omits was
        // removed, archived or made server-only, so it now serves the
        // fallback. A failed request keeps the bootstrap values.
        this.bootstrap.clear();
        this.currentStatus = "ready";
      } catch (error) {
        if (generation !== this.generation) return;
        this.currentStatus = "error";
        this.onError(error instanceof Error ? error : new Error(String(error)));
      }
      this.notify();
    })();
    this.inflight = request;
    return request;
  }

  private notify(): void {
    this.revision += 1;
    for (const listener of [...this.listeners]) {
      try {
        listener();
      } catch {
        // A failing subscriber must not stop the others.
      }
    }
  }

  private recordExposure(evaluation: FlagEvaluation): void {
    if (!evaluation.record_exposure || !this.onExposure) return;
    const variant = evaluation.variant ?? "";
    const marker = `${evaluation.key}\u0000${variant}`;
    if (this.exposed.has(marker)) return;
    this.exposed.add(marker);
    try {
      this.onExposure({ flag_key: evaluation.key, variant, reason: evaluation.reason.kind });
    } catch {
      // Exposure reporting is best effort.
    }
  }
}

/** Serve `value` only if it has the fallback's shape, so a typed call site
 * never receives a string where it expected a boolean. */
function sameType<T>(value: unknown, fallback: T): T {
  return compatible(value, fallback) ? (value as T) : fallback;
}

/** Whether `value` can be served where `fallback`'s type is expected. */
function compatible(value: unknown, fallback: unknown): boolean {
  if (fallback === null || fallback === undefined) return true;
  if (value === null || value === undefined) return false;
  if (typeof value !== typeof fallback) return false;
  return Array.isArray(fallback) === Array.isArray(value);
}

/** Event data for a `flag_exposure` analytics event. */
export function exposureEventData(exposure: FlagExposure): Record<string, JsonValue> {
  return { flag_key: exposure.flag_key, variant: exposure.variant, reason: exposure.reason };
}

/** A context as a value: attributes compared by content, not key order. */
function contextKey(context: FlagContext): string {
  const attributes = Object.entries(context.attributes ?? {}).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  return JSON.stringify([context.key ?? null, attributes]);
}
