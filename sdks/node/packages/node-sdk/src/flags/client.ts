/**
 * In-memory feature-flag client for services running on Temps.
 *
 * Reads are synchronous and cost no network I/O: the whole environment's flag
 * set lives in memory and is refreshed in the background. That is the point —
 * a per-check HTTP round trip (~200-500ms) is unusable in a request handler.
 *
 * Zero configuration on Temps: `TEMPS_API_URL` and `TEMPS_API_TOKEN` are
 * injected into every deployment, and the token pins the project (and usually
 * the environment), so `new FlagsClient()` already knows what to fetch.
 */

import { evaluate } from './evaluate';
import type {
  EvalContext,
  Evaluation,
  FlagSnapshot,
  FlagSnapshotResponse,
  FlagsClientOptions,
} from './types';

const DEFAULT_REFRESH_INTERVAL_MS = 30_000;
const DEFAULT_TIMEOUT_MS = 5_000;
/**
 * Matches `MAX_EXPOSURE_KEYS` on the server, which now rejects (rather than
 * truncates) an over-sized batch. A project with more flags than this must be
 * chunked, or its overflow keys would never be stamped.
 */
const MAX_EXPOSURE_KEYS = 500;

/**
 * Read an environment variable without depending on Node type definitions.
 *
 * This package is consumed from browsers too, so `lib` is DOM-flavoured and
 * there is no `process` global in the type environment. Going through
 * `globalThis` keeps the client usable in both without pulling `@types/node`
 * into every consumer.
 */
function envVar(name: string): string | undefined {
  const global = globalThis as { process?: { env?: Record<string, string | undefined> } };
  return global.process?.env?.[name];
}

export class FlagsClient {
  private readonly apiUrl: string;
  private readonly apiToken: string;
  private readonly environmentId?: number;
  private readonly refreshIntervalMs: number;
  private readonly timeoutMs: number;
  private readonly reportExposure: boolean;
  private readonly onError: (error: Error) => void;

  private flags = new Map<string, FlagSnapshot>();
  private etag: string | null = null;
  private timer: ReturnType<typeof setInterval> | null = null;
  /**
   * Keys actually evaluated since the last report.
   *
   * Only keys present in the snapshot are recorded, which bounds this by the
   * project's flag count — a caller doing `get('user-' + id, ...)` cannot grow
   * it without limit.
   */
  private evaluated = new Set<string>();

  /** True once a snapshot has been successfully loaded at least once. */
  public ready = false;
  /**
   * The most recent refresh failure, or `null`. Exposed rather than swallowed:
   * a self-hosted operator debugging alone needs to see that flags are stale,
   * not guess why a rollout did nothing.
   */
  public lastError: Error | null = null;

  constructor(options: FlagsClientOptions = {}) {
    this.apiUrl = (options.apiUrl ?? envVar('TEMPS_API_URL') ?? '').replace(/\/+$/, '');
    this.apiToken = options.apiToken ?? envVar('TEMPS_API_TOKEN') ?? '';
    this.environmentId = options.environmentId;
    this.refreshIntervalMs = options.refreshIntervalMs ?? DEFAULT_REFRESH_INTERVAL_MS;
    this.timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
    this.reportExposure = options.reportExposure ?? true;
    this.onError =
      options.onError ??
      ((error) => {
        console.warn(`[temps-flags] ${error.message}`);
      });
  }

  /**
   * Load the first snapshot and start background refresh.
   *
   * Never throws. A flag service that is unreachable must not stop an app from
   * booting — every `get()` returns its caller-supplied fallback until a
   * snapshot arrives, and `ready` stays `false` so you can surface it.
   */
  async init(): Promise<void> {
    if (!this.apiUrl || !this.apiToken) {
      this.fail(
        new Error(
          'TEMPS_API_URL and TEMPS_API_TOKEN are required (both are injected automatically for apps deployed on Temps). Flags will serve fallbacks.',
        ),
      );
    } else {
      await this.refresh();
    }

    if (this.timer === null && this.refreshIntervalMs > 0) {
      this.timer = setInterval(() => {
        void this.refresh();
        void this.flushExposure();
      }, this.refreshIntervalMs);
      // On Node, don't hold the process open just to poll for flags. No-op in
      // a browser, where timer handles are plain numbers.
      (this.timer as unknown as { unref?: () => void }).unref?.();
    }
  }

  /** Stop background refresh. Safe to call more than once. */
  close(): void {
    if (this.timer !== null) {
      clearInterval(this.timer);
      this.timer = null;
    }
    // Best-effort final report so a graceful shutdown doesn't discard the
    // last interval's worth of usage.
    void this.flushExposure();
  }

  /**
   * Read a flag. Synchronous, in-memory, no network I/O.
   *
   * `fallback` is returned whenever the flag cannot be resolved — unknown key,
   * snapshot not loaded yet, or a stored value that does not match the flag's
   * declared type.
   */
  get<T>(key: string, context: EvalContext = {}, fallback: T): T {
    return this.getDetails<T>(key, context, fallback).value;
  }

  /**
   * Read a flag along with the reason it resolved that way.
   *
   * Use this when debugging "why is this user seeing the old checkout?" — the
   * reason distinguishes a kill switch from an unset value from a flag that
   * does not exist.
   */
  getDetails<T>(key: string, context: EvalContext = {}, fallback: T): Evaluation<T> {
    const flag = this.flags.get(key);

    // Note the key in memory only — reporting happens on the refresh interval.
    // Writing per evaluation would put a network call in the caller's hot path,
    // which is the entire thing this client exists to avoid.
    if (flag && this.reportExposure) {
      this.evaluated.add(key);
    }

    return evaluate<T>(flag, context, fallback);
  }

  /** Every flag key currently cached. */
  keys(): string[] {
    return [...this.flags.keys()];
  }

  /**
   * Fetch the snapshot once.
   *
   * A failure leaves the previous snapshot in place: stale flags are far better
   * than every flag suddenly reverting to its fallback because the control
   * plane blinked.
   */
  async refresh(): Promise<void> {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), this.timeoutMs);

    try {
      const url = new URL(`${this.apiUrl}/flags/snapshot`);
      if (this.environmentId !== undefined) {
        url.searchParams.set('environment_id', String(this.environmentId));
      }

      const headers: Record<string, string> = {
        Authorization: `Bearer ${this.apiToken}`,
        Accept: 'application/json',
      };
      if (this.etag) {
        headers['If-None-Match'] = this.etag;
      }

      const response = await fetch(url, { headers, signal: controller.signal });

      // Nothing changed since the last poll — the common case.
      if (response.status === 304) {
        this.lastError = null;
        return;
      }

      if (!response.ok) {
        this.fail(
          new Error(
            `Flag snapshot request failed: ${response.status} ${response.statusText}. Serving ${this.ready ? 'the last known' : 'fallback'} values.`,
          ),
        );
        return;
      }

      const body = (await response.json()) as FlagSnapshotResponse;

      const next = new Map<string, FlagSnapshot>();
      for (const flag of body.flags ?? []) {
        next.set(flag.key, flag);
      }

      this.flags = next;
      this.etag = response.headers.get('etag');
      this.ready = true;
      this.lastError = null;
    } catch (error) {
      const reason = error instanceof Error ? error.message : String(error);
      this.fail(
        new Error(
          `Flag snapshot request failed: ${reason}. Serving ${this.ready ? 'the last known' : 'fallback'} values.`,
        ),
      );
    } finally {
      clearTimeout(timeout);
    }
  }

  /**
   * Report which flags were actually evaluated since the last flush.
   *
   * This is what gives `last evaluated` in the console a real meaning: the
   * server hands out the whole flag set and evaluation happens here, so
   * without this it could only ever know that *something* fetched the list.
   *
   * Best-effort. A failure drops the batch rather than retrying forever —
   * usage telemetry must never grow unboundedly or interfere with reads.
   */
  async flushExposure(): Promise<void> {
    if (!this.reportExposure || this.evaluated.size === 0) return;
    if (!this.apiUrl || !this.apiToken) return;

    // Take the batch up front so evaluations during the request aren't lost.
    const pending = [...this.evaluated];
    this.evaluated.clear();

    // Chunk to the server's cap. It rejects over-sized batches outright, so a
    // project with >500 flags would otherwise have its whole report refused
    // and those flags would read as "never evaluated" forever.
    for (let i = 0; i < pending.length; i += MAX_EXPOSURE_KEYS) {
      const keys = pending.slice(i, i + MAX_EXPOSURE_KEYS);
      const controller = new AbortController();
      const timeout = setTimeout(() => controller.abort(), this.timeoutMs);

      try {
        const response = await fetch(`${this.apiUrl}/flags/exposure`, {
          method: 'POST',
          headers: {
            Authorization: `Bearer ${this.apiToken}`,
            'Content-Type': 'application/json',
          },
          body: JSON.stringify({ keys }),
          signal: controller.signal,
        });

        // Put a rejected batch back so the next flush retries it. Bounded by
        // the flag count, so this cannot grow without limit.
        if (!response.ok) {
          for (const key of keys) this.evaluated.add(key);
        }
      } catch {
        // Deliberately silent: a dropped usage report is not worth a warning
        // in the app's logs, and `lastError` is reserved for the failure that
        // actually matters (flags going stale).
        for (const key of keys) this.evaluated.add(key);
      } finally {
        clearTimeout(timeout);
      }
    }
  }

  private fail(error: Error): void {
    this.lastError = error;
    this.onError(error);
  }
}

/**
 * Process-wide client, for the common case where an app has one flag set.
 *
 * ```ts
 * import { flags } from '@temps-sdk/node-sdk';
 *
 * await flags.init();
 * if (flags.get('checkout.v2', { key: user.id }, false)) { ... }
 * ```
 */
export const flags = new FlagsClient();
