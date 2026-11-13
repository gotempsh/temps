// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { JsonValue, WaitResult } from './protocol.js';

/** Milliseconds, or a string such as `"30s"`, `"5m"`, `"2h"`, `"1d"`. */
export type Duration = number | `${number}${'ms' | 's' | 'm' | 'h' | 'd'}`;

export interface RetryPolicy {
  /** Retries after the first attempt; default 3 (4 attempts in all). */
  retries?: number;
  /** First backoff; default 1 s. */
  initialBackoff?: Duration;
  /** Longest backoff; default 5 min. */
  maxBackoff?: Duration;
  /** Backoff growth per attempt; default 2. */
  factor?: number;
}

export interface StepOptions extends RetryPolicy {
  /** Abort an attempt after this long; the attempt then fails and is retried. */
  timeout?: Duration;
}

export interface StepAttempt {
  /** 1 for the first attempt. */
  attempt: number;
  /** Aborted when the attempt times out. */
  signal: AbortSignal;
}

export interface WaitOptions {
  /** The event name, as given to `sendEvent`. */
  event: string;
  /** Only an event sent with this key wakes the run. */
  key?: string;
  /** Give up after this long; the result then has `received: false`. */
  timeout?: Duration;
}

export interface SendEventOptions {
  key?: string;
  payload?: JsonValue;
}

/**
 * What a workflow function receives. Every method is durable: its result is
 * recorded in the run's journal and replayed, never recomputed, when the
 * workflow function runs again after a restart, a sleep or a deploy.
 *
 * The contract: step bodies are idempotent (they run at least once), step
 * names are stable, and the workflow function uses no wall-clock time or
 * randomness outside `ctx.now()` and `ctx.random()`.
 */
export interface WorkflowContext<Input = JsonValue> {
  readonly runId: string;
  readonly workflow: string;
  /** 1 on the run's first invocation. */
  readonly invocation: number;
  readonly input: Input;
  step<T>(name: string, fn: (attempt: StepAttempt) => Promise<T> | T, options?: StepOptions): Promise<T>;
  sleep(name: string, duration: Duration): Promise<void>;
  sleepUntil(name: string, until: Date | string): Promise<void>;
  waitForEvent<T = JsonValue>(name: string, options: WaitOptions): Promise<WaitResult<T>>;
  sendEvent(name: string, event: string, options?: SendEventOptions): Promise<void>;
  /** The time, recorded once and replayed. */
  now(name?: string): Promise<Date>;
  /** A number in [0, 1), recorded once and replayed. */
  random(name?: string): Promise<number>;
}

export interface WorkflowOptions {
  /** A label of this workflow's code version, shown on runs. */
  version?: string;
  /** Runs of this workflow allowed to execute at once in the environment. */
  concurrency?: number;
}

export interface WorkflowDefinition<Input = any, Output = any> {
  readonly name: string;
  readonly options: WorkflowOptions;
  readonly run: (ctx: WorkflowContext<Input>) => Promise<Output>;
}

/** The server's rule for workflow and step names (`validate_name`). */
const NAME_CHARS = /^[A-Za-z0-9._:/-]+$/;

export function checkName(what: string, value: string, max: number): void {
  if (value.length === 0 || value.length > max || !NAME_CHARS.test(value)) {
    throw new Error(
      `${what} '${value}' is invalid: use 1-${max} letters, digits, '.', '_', '-', ':' or '/'`,
    );
  }
}

/**
 * Define a workflow. Pass the definitions to `createWorkflowHandler`, and
 * start runs with `WorkflowsClient.start(name, input)`.
 *
 * ```ts
 * export const sendInvoice = defineWorkflow('send-invoice', async (ctx) => {
 *   const invoice = await ctx.step('create', () => billing.create(ctx.input.orderId));
 *   await ctx.sleep('grace-period', '3d');
 *   const paid = await ctx.waitForEvent('payment', { event: 'invoice.paid', key: invoice.id, timeout: '7d' });
 *   if (!paid.received) await ctx.step('remind', () => mail.remind(invoice.id));
 * });
 * ```
 */
/** The largest concurrency Temps accepts in a manifest. */
export const MAX_CONCURRENCY = 1_000_000;

export function defineWorkflow<Input = any, Output = any>(
  name: string,
  run: (ctx: WorkflowContext<Input>) => Promise<Output>,
  options: WorkflowOptions = {},
): WorkflowDefinition<Input, Output> {
  checkName('Workflow name', name, 128);
  if (
    options.concurrency !== undefined &&
    (!Number.isInteger(options.concurrency) || options.concurrency < 1 || options.concurrency > MAX_CONCURRENCY)
  ) {
    throw new Error(`Workflow '${name}': concurrency must be an integer from 1 to ${MAX_CONCURRENCY}`);
  }
  return Object.freeze({ name, options: { ...options }, run });
}

const UNITS: Record<string, number> = { ms: 1, s: 1000, m: 60_000, h: 3_600_000, d: 86_400_000 };

export function durationMs(duration: Duration): number {
  if (typeof duration === 'number') {
    if (!Number.isFinite(duration) || duration < 0) throw new Error(`Invalid duration ${duration}`);
    return duration;
  }
  const match = /^(\d+(?:\.\d+)?)(ms|s|m|h|d)$/.exec(duration);
  if (!match) throw new Error(`Invalid duration '${duration}': use a number of ms or e.g. "30s", "5m", "2h", "1d"`);
  return Number(match[1]) * (UNITS[match[2] as string] as number);
}
