// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Replay: run one invocation of a workflow against its journal.
 *
 * The workflow function runs from the top on every invocation. Each durable
 * call (`step`, `sleep`, `waitForEvent`, `sendEvent`, `now`, `random`) takes
 * the next journal sequence: a recorded entry is replayed (its result
 * returned, its error thrown), a new one is executed and recorded with Temps
 * **before** the function continues. The invocation ends when the function
 * returns or throws (completed/failed), or when it reaches something it must
 * wait for (a sleep, an event, a retry backoff): it then suspends, and Temps
 * invokes the run again when it is due. Nothing stays in memory between
 * invocations.
 */

import { decode, decodeEntry, encodeOutcome, encodeWrite, PayloadCodecError, type PayloadCodec } from './codec.js';
import { randomBytes } from 'node:crypto';

import {
  RunCancelledError,
  StaleLeaseError,
  StepFailedError,
  TempsApiError,
  TempsUnavailableError,
  WorkflowIncompatibleError,
  errorMessage,
} from './errors.js';
import {
  CHILD_EVENT,
  RESERVED_EVENT_PREFIX,
  type ChildResult,
  type InvokeRequest,
  type JournalEntry,
  type JsonValue,
  type Outcome,
  type StepKind,
  type StepWriteRequest,
  type WaitResult,
} from './protocol.js';
import { StepAttemptError, type Telemetry } from './telemetry.js';
import type { ExecutorTransport } from './transport.js';
import {
  checkName,
  durationMs,
  type Duration,
  type SendEventOptions,
  type StepAttempt,
  type StepOptions,
  type WaitOptions,
  type WorkflowContext,
  type WorkflowDefinition,
} from './workflow.js';

const MAX_STEP_NAME = 200;
/**
 * Stop starting new steps this long before the invocation deadline, or a
 * quarter of the invocation's window when that is shorter, so a short
 * deadline still leaves room to work.
 */
const DEADLINE_MARGIN_MS = 30_000;
const MAX_ERROR_LENGTH = 16_000;

/** How an invocation ended, for the handler. */
export type InvocationResult =
  | { type: 'outcome'; outcome: Outcome }
  /** The run belongs to another lease now: answer 202, report nothing. */
  | { type: 'stale' }
  /** Temps could not be reached: answer 503, so Temps retries the invocation. */
  | { type: 'unavailable'; reason: string };

export interface InvocationDeps {
  codec?: PayloadCodec;
  transport: ExecutorTransport;
  telemetry: Telemetry;
  token: string;
  /** Heartbeat interval override (tests); default a third of the lease. */
  heartbeatMs?: number;
}

/** Thrown to unwind the workflow function at a suspension point. */
class Suspension {
  constructor(public readonly outcome: Outcome) {}
}

/** A user-level `catch` must not swallow a suspension or a lease loss. */
function isControl(error: unknown): boolean {
  return (
    error instanceof PayloadCodecError ||
    error instanceof Suspension ||
    error instanceof StaleLeaseError ||
    error instanceof WorkflowIncompatibleError ||
    error instanceof RunCancelledError ||
    error instanceof TempsUnavailableError
  );
}

/** A call to Temps that failed for Temps' reasons (unreachable, 5xx), not the run's. */
function isOutage(error: unknown): error is TempsApiError {
  return error instanceof TempsApiError && (error.status === 0 || error.status >= 500);
}

function toJson(value: unknown, what: string): JsonValue {
  if (value === undefined) return null;
  try {
    return JSON.parse(JSON.stringify(value)) as JsonValue;
  } catch (error) {
    throw new Error(`${what} is not JSON-serializable: ${errorMessage(error)}`);
  }
}

function clip(message: string): string {
  return message.length > MAX_ERROR_LENGTH ? `${message.slice(0, MAX_ERROR_LENGTH)}…` : message;
}

function checkUnreserved(event: string): void {
  if (event.startsWith(RESERVED_EVENT_PREFIX)) {
    throw new Error(`Event name '${event}' starts with '${RESERVED_EVENT_PREFIX}', which is reserved for events Temps sends itself`);
  }
}

export function backoffMs(options: StepOptions, attempt: number): number {
  const initial = durationMs(options.initialBackoff ?? 1000);
  const max = durationMs(options.maxBackoff ?? 300_000);
  const factor = options.factor ?? 2;
  return Math.min(max, initial * factor ** Math.max(0, attempt - 1));
}

class Invocation<Input> {
  private readonly journal = new Map<number, JournalEntry>();
  private sequence = 0;
  private readonly startedAt = performance.now();
  private readonly tempsStart: number;
  private readonly deadline: number;
  private readonly margin: number;
  private cancelled = false;
  private stale = false;
  /**
   * The first control signal raised (a suspension, a lost lease, an
   * incompatibility). Sticky: a user `catch` that swallows it cannot make
   * the workflow continue, since every later durable call raises it again
   * and it decides the outcome even if the function then returns.
   */
  private control?: unknown;
  private heartbeat?: ReturnType<typeof setInterval>;
  private heartbeatInFlight = false;
  /** Hidden from user code: the context is what the workflow sees. */
  readonly context: WorkflowContext<Input>;

  constructor(
    private readonly definition: WorkflowDefinition<Input>,
    private readonly request: InvokeRequest,
    private readonly deps: InvocationDeps,
  ) {
    for (const entry of request.journal) this.journal.set(entry.sequence, entry);
    this.tempsStart = Date.parse(request.now);
    this.deadline = Date.parse(request.deadline_at);
    this.margin = Math.min(DEADLINE_MARGIN_MS, Math.max(0, (this.deadline - this.tempsStart) / 4));
    const self = this;
    this.context = {
      runId: request.run_id,
      workflow: request.workflow,
      invocation: request.attempt,
      input: (request.input ?? null) as Input,
      step: (name, fn, options) => self.step(name, fn, options),
      sleep: (name, duration) => self.sleep(name, durationMs(duration)),
      sleepUntil: (name, until) => self.sleepUntil(name, until),
      waitForEvent: (name, options) => self.waitForEvent(name, options),
      sendEvent: (name, event, options) => self.sendEvent(name, event, options),
      child: (name, workflow, input) => self.child(name, workflow, input),
      now: (name) => self.now(name ?? 'now'),
      random: (name) => self.random(name ?? 'random'),
    };
  }

  /** Temps' clock: the invoke request's `now` plus the time spent since. */
  private clock(): number {
    return this.tempsStart + (performance.now() - this.startedAt);
  }

  async run(): Promise<InvocationResult> {
    try {
      await this.loadJournal();
      this.startHeartbeat();
      const output = await this.definition.run(this.context);
      if (this.control !== undefined) return this.ended(this.control);
      this.checkLease();
      const unread = [...this.journal.values()].find((entry) => entry.sequence >= this.sequence);
      if (unread) throw new WorkflowIncompatibleError(this.request.run_id, unread.sequence, `${unread.kind} '${unread.name}'`, 'end of workflow');
      return { type: 'outcome', outcome: { type: 'completed', output: toJson(output, 'The workflow output') } };
    } catch (error) {
      return this.ended(error);
    } finally {
      if (this.heartbeat) clearInterval(this.heartbeat);
    }
  }

  private ended(thrown: unknown): InvocationResult {
    const error = this.control ?? thrown;
    if (error instanceof PayloadCodecError) return { type: 'unavailable', reason: error.message };
    if (error instanceof Suspension) return { type: 'outcome', outcome: error.outcome };
    if (error instanceof StaleLeaseError) return { type: 'stale' };
    if (error instanceof TempsUnavailableError) return { type: 'unavailable', reason: error.message };
    if (error instanceof WorkflowIncompatibleError) {
      return {
        type: 'outcome',
        outcome: { type: 'incompatible', sequence: error.sequence, expected: error.expected, found: error.found },
      };
    }
    if (this.stale) return { type: 'stale' };
    if (error instanceof RunCancelledError) {
      return { type: 'outcome', outcome: { type: 'failed', error: error.message } };
    }
    if (error instanceof StepFailedError) {
      return { type: 'outcome', outcome: { type: 'failed', error: clip(error.message), sequence: error.sequence } };
    }
    return { type: 'outcome', outcome: { type: 'failed', error: clip(errorMessage(error)) } };
  }

  private async loadJournal(): Promise<void> {
    let next = this.request.journal_next_sequence ?? null;
    while (next !== null && next !== undefined) {
      let page;
      try {
        page = await this.deps.transport.journal(this.deps.token, this.request.run_id, next);
      } catch (error) {
        if (isOutage(error)) this.raise(new TempsUnavailableError(this.request.run_id, error));
        throw error;
      }
      for (const entry of page.entries) this.journal.set(entry.sequence, this.deps.codec ? await decodeEntry(this.deps.codec, entry) : entry);
      next = page.next_sequence ?? null;
    }
  }

  private startHeartbeat(): void {
    const leaseMs = Date.parse(this.request.lease_expires_at) - this.tempsStart;
    const interval = this.deps.heartbeatMs ?? Math.max(1000, Math.floor(leaseMs / 3));
    this.heartbeat = setInterval(() => {
      // One at a time: a slow Temps must not pile heartbeats up.
      if (this.heartbeatInFlight) return;
      this.heartbeatInFlight = true;
      this.deps.transport
        .heartbeat(this.deps.token, this.request.run_id)
        .then(
          (reply) => {
            if (reply.cancelled) this.cancelled = true;
          },
          (error: unknown) => {
            if (error instanceof StaleLeaseError) this.stale = true;
          },
        )
        .finally(() => {
          this.heartbeatInFlight = false;
        });
    }, interval);
    // Never keep the process alive for a heartbeat.
    (this.heartbeat as { unref?: () => void }).unref?.();
  }

  /** Record `signal` as the invocation's control signal and throw it. */
  private raise(signal: unknown): never {
    if (this.control === undefined) this.control = signal;
    throw this.control;
  }

  private checkLease(): void {
    if (this.control !== undefined) throw this.control;
    if (this.stale) this.raise(new StaleLeaseError(this.request.run_id, 'the lease was lost'));
    if (this.cancelled) this.raise(new RunCancelledError(this.request.run_id));
  }

  /** Claim the next sequence for `name`/`kind`; returns its recorded entry. */
  private next(name: string, kind: StepKind): { sequence: number; entry?: JournalEntry } {
    checkName('Step name', name, MAX_STEP_NAME);
    this.checkLease();
    const sequence = this.sequence;
    this.sequence += 1;
    const entry = this.journal.get(sequence);
    if (entry && (entry.name !== name || entry.kind !== kind)) {
      this.raise(new WorkflowIncompatibleError(
        this.request.run_id,
        sequence,
        `${entry.kind} '${entry.name}'`,
        `${kind} '${name}'`,
      ));
    }
    return { sequence, entry };
  }

  /** Before running new work: suspend and continue in a fresh invocation if the deadline is near. */
  private yieldIfLate(sequence: number): void {
    if (this.clock() >= this.deadline - this.margin) {
      this.raise(new Suspension({ type: 'retry', sequence, at: new Date(this.clock()).toISOString() }));
    }
  }

  private async record(write: StepWriteRequest): Promise<JournalEntry> {
    let reply;
    try {
      reply = await this.deps.transport.recordStep(this.deps.token, this.request.run_id, this.deps.codec ? await encodeWrite(this.deps.codec, write) : write);
      if (this.deps.codec) reply = { ...reply, entry: await decodeEntry(this.deps.codec, reply.entry) };
    } catch (error) {
      if (error instanceof PayloadCodecError || error instanceof StaleLeaseError || error instanceof WorkflowIncompatibleError) this.raise(error);
      if (isOutage(error)) this.raise(new TempsUnavailableError(this.request.run_id, error));
      throw error;
    }
    this.journal.set(reply.entry.sequence, reply.entry);
    return reply.entry;
  }

  private replayed<T>(entry: JournalEntry, name: string): T {
    if (entry.status === 'failed') {
      throw new StepFailedError(name, entry.sequence, entry.attempts, entry.error ?? 'failed');
    }
    return (entry.result ?? null) as T;
  }

  async step<T>(name: string, fn: (attempt: StepAttempt) => Promise<T> | T, options: StepOptions = {}): Promise<T> {
    const { sequence, entry } = this.next(name, 'step');
    if (entry && entry.status !== 'pending') return this.replayed<T>(entry, name);
    this.yieldIfLate(sequence);
    const attempt = (entry?.attempts ?? 0) + 1;
    const maxAttempts = Math.max(1, (options.retries ?? 3) + 1);
    const telemetryContext = { runId: this.request.run_id, workflow: this.request.workflow, step: name, sequence, attempt };
    const started = performance.now();
    let value: T;
    let traceId: string | undefined;
    try {
      const result = await this.deps.telemetry.inSpan(telemetryContext, () => this.attempt(fn, attempt, options));
      value = result.value;
      traceId = result.traceId;
    } catch (thrown) {
      const error = thrown instanceof StepAttemptError ? thrown.cause : thrown;
      if (isControl(error)) throw error;
      const failedTraceId = thrown instanceof StepAttemptError ? thrown.traceId : undefined;
      return this.attemptFailed<T>(name, sequence, attempt, maxAttempts, options, error, {
        duration: Math.round(performance.now() - started),
        traceId: failedTraceId,
        telemetryContext,
      });
    }
    const recorded = await this.record({
      sequence,
      name,
      kind: 'step',
      status: 'completed',
      attempts: attempt,
      result: toJson(value, `The result of step '${name}'`),
      duration_ms: Math.round(performance.now() - started),
      trace_id: traceId ?? null,
    });
    // Another invocation recorded this step first: continue with its value.
    return this.replayed<T>(recorded, name);
  }

  private async attempt<T>(fn: (attempt: StepAttempt) => Promise<T> | T, attempt: number, options: StepOptions): Promise<T> {
    const controller = new AbortController();
    if (options.timeout === undefined) return fn({ attempt, signal: controller.signal });
    const timeoutMs = durationMs(options.timeout);
    let timer: ReturnType<typeof setTimeout> | undefined;
    const timedOut = new Promise<never>((_, reject) => {
      timer = setTimeout(() => {
        controller.abort();
        reject(new Error(`Step attempt timed out after ${timeoutMs} ms`));
      }, timeoutMs);
    });
    try {
      return await Promise.race([Promise.resolve(fn({ attempt, signal: controller.signal })), timedOut]);
    } finally {
      clearTimeout(timer);
    }
  }

  private async attemptFailed<T>(
    name: string,
    sequence: number,
    attempt: number,
    maxAttempts: number,
    options: StepOptions,
    error: unknown,
    meta: { duration: number; traceId?: string; telemetryContext: Parameters<Telemetry['capture']>[1] },
  ): Promise<T> {
    const message = clip(errorMessage(error));
    const errorRef = await this.deps.telemetry.capture(error, meta.telemetryContext);
    const final = attempt >= maxAttempts;
    const recorded = await this.record({
      sequence,
      name,
      kind: 'step',
      status: final ? 'failed' : 'pending',
      attempts: attempt,
      error: message,
      duration_ms: meta.duration,
      trace_id: meta.traceId ?? null,
      error_ref: errorRef ?? null,
    });
    if (recorded.status === 'completed') return (recorded.result ?? null) as T;
    if (recorded.status === 'failed') {
      throw new StepFailedError(name, sequence, recorded.attempts, recorded.error ?? message);
    }
    const at = new Date(this.clock() + backoffMs(options, attempt)).toISOString();
    this.raise(new Suspension({ type: 'retry', sequence, at }));
  }

  async sleep(name: string, ms: number): Promise<void> {
    const { sequence, entry } = this.next(name, 'sleep');
    if (entry?.status === 'completed') return;
    const wakeAt = entry?.wake_at ? Date.parse(entry.wake_at) : this.clock() + ms;
    await this.sleepTo(name, sequence, wakeAt, entry !== undefined);
  }

  async sleepUntil(name: string, until: Date | string): Promise<void> {
    const target = typeof until === 'string' ? Date.parse(until) : until.getTime();
    if (!Number.isFinite(target)) throw new Error(`sleepUntil '${name}': invalid date`);
    const { sequence, entry } = this.next(name, 'sleep');
    if (entry?.status === 'completed') return;
    const wakeAt = entry?.wake_at ? Date.parse(entry.wake_at) : target;
    await this.sleepTo(name, sequence, wakeAt, entry !== undefined);
  }

  private async sleepTo(name: string, sequence: number, wakeAt: number, pending: boolean): Promise<void> {
    if (this.clock() >= wakeAt) {
      await this.record({ sequence, name, kind: 'sleep', status: 'completed', attempts: 1 });
      return;
    }
    const until = new Date(wakeAt).toISOString();
    if (!pending) {
      await this.record({ sequence, name, kind: 'sleep', status: 'pending', attempts: 1, wake_at: until });
    }
    this.raise(new Suspension({ type: 'sleep', sequence, until }));
  }

  async waitForEvent<T>(name: string, options: WaitOptions): Promise<WaitResult<T>> {
    checkName('Event name', options.event, 200);
    checkUnreserved(options.event);
    const { sequence, entry } = this.next(name, 'wait');
    if (entry?.status === 'completed') return (entry.result ?? { received: false, payload: null }) as unknown as WaitResult<T>;
    const key = options.key ?? null;
    let timeoutAt = entry?.wake_at ?? null;
    if (!entry) {
      timeoutAt = options.timeout === undefined ? null : new Date(this.clock() + durationMs(options.timeout)).toISOString();
      await this.record({ sequence, name, kind: 'wait', status: 'pending', attempts: 1, wake_at: timeoutAt });
    } else if (timeoutAt && this.clock() >= Date.parse(timeoutAt)) {
      const recorded = await this.record({
        sequence,
        name,
        kind: 'wait',
        status: 'completed',
        attempts: entry.attempts,
        result: { received: false, payload: null },
      });
      // The event may have won the race: the recorded value is the answer.
      return (recorded.result ?? { received: false, payload: null }) as unknown as WaitResult<T>;
    }
    this.raise(new Suspension({ type: 'wait', sequence, event: options.event, key, timeout_at: timeoutAt }));
  }

  async child<T>(name: string, workflow: string, input?: JsonValue): Promise<ChildResult<T>> {
    checkName('Workflow name', workflow, 128);
    const { sequence, entry } = this.next(name, 'child');
    if (entry?.status === 'completed') return entry.result as unknown as ChildResult<T>;
    let pending = entry;
    if (!pending) {
      pending = await this.record({
        sequence,
        name,
        kind: 'child',
        status: 'pending',
        attempts: 1,
        child: { workflow, input: toJson(input, `The input of child '${name}'`) },
      });
      // The child may have finished, and Temps recorded its result, already.
      if (pending.status === 'completed') return pending.result as unknown as ChildResult<T>;
    }
    const runId = (pending.result as { run_id?: unknown } | null)?.run_id;
    if (typeof runId !== 'string') {
      throw new Error(`child '${name}' (entry ${sequence}) has no child run id recorded`);
    }
    this.raise(new Suspension({ type: 'wait', sequence, event: CHILD_EVENT, key: runId, timeout_at: null }));
  }

  async sendEvent(name: string, event: string, options: SendEventOptions = {}): Promise<void> {
    checkName('Event name', event, 200);
    checkUnreserved(event);
    const { sequence, entry } = this.next(name, 'send_event');
    if (entry) return;
    await this.record({
      sequence,
      name,
      kind: 'send_event',
      status: 'completed',
      attempts: 1,
      event: { name: event, key: options.key ?? null, payload: options.payload ?? null },
    });
  }

  async now(name: string): Promise<Date> {
    const { sequence, entry } = this.next(name, 'now');
    if (entry) return new Date(String(entry.result));
    const recorded = await this.record({
      sequence,
      name,
      kind: 'now',
      status: 'completed',
      attempts: 1,
      result: new Date(this.clock()).toISOString(),
    });
    return new Date(String(recorded.result));
  }

  async random(name: string): Promise<number> {
    const { sequence, entry } = this.next(name, 'random');
    if (entry) return Number(entry.result);
    // 53 random bits, as Math.random would give, from the CSPRNG.
    const bits = randomBytes(8).readBigUInt64BE() >> 11n;
    const value = Number(bits) / 2 ** 53;
    const recorded = await this.record({ sequence, name, kind: 'random', status: 'completed', attempts: 1, result: value });
    return Number(recorded.result);
  }
}

/** Run one invocation of `definition` for `request`. Never throws. */
export async function runInvocation<Input>(
  definition: WorkflowDefinition<Input>,
  request: InvokeRequest,
  deps: InvocationDeps,
): Promise<InvocationResult> {
  try {
    const decoded = deps.codec ? { ...request,
      input: await decode(deps.codec, request.input ?? null),
      journal: await Promise.all(request.journal.map((entry) => decodeEntry(deps.codec!, entry))),
    } : request;
    const result = await new Invocation(definition, decoded, deps).run();
    return deps.codec && result.type === 'outcome'
      ? { ...result, outcome: await encodeOutcome(deps.codec, result.outcome) } : result;
  } catch {
    return { type: 'unavailable', reason: new PayloadCodecError().message };
  }
}

export type { Duration };
