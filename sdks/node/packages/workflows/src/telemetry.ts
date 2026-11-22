// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Tracing and error tracking for step attempts.
 *
 * Each step attempt runs in an OpenTelemetry span (`workflow.step <name>`)
 * when `@opentelemetry/api` is installed and the app registered a tracer
 * provider; the span's trace id is recorded on the journal entry so the
 * console links the step to its trace. A failed attempt is passed to the
 * app's error tracker (any Sentry-compatible `captureException`, which Temps
 * error tracking accepts), and the returned event id is recorded too.
 * Attributes carry names and ids only, never inputs or results.
 */

export interface SpanLike {
  setAttribute(key: string, value: string | number | boolean): unknown;
  recordException(exception: Error): unknown;
  setStatus(status: { code: number; message?: string }): unknown;
  spanContext(): { traceId: string };
  end(): void;
}

export interface TracerLike {
  startActiveSpan<T>(name: string, fn: (span: SpanLike) => T): T;
}

export interface ErrorTracker {
  /** Returns the event id, if the tracker reports one. */
  captureException(
    error: unknown,
    context?: { tags?: Record<string, string>; extra?: Record<string, unknown> },
  ): string | undefined | void | Promise<string | undefined | void>;
}

export interface TelemetryOptions {
  /** Omit application error messages/stacks from telemetry (enabled by app-held codecs). */
  redactErrors?: boolean;
  /** A tracer; `false` disables tracing. Default: `@opentelemetry/api`'s, if installed. */
  tracer?: TracerLike | false;
  errorTracker?: ErrorTracker;
}

export interface StepTelemetryContext {
  runId: string;
  workflow: string;
  step: string;
  sequence: number;
  attempt: number;
}

const INVALID_TRACE_ID = '00000000000000000000000000000000';
const SPAN_STATUS_ERROR = 2;

export class Telemetry {
  private tracer: Promise<TracerLike | undefined>;

  constructor(private readonly options: TelemetryOptions = {}) {
    if (options.tracer === false) {
      this.tracer = Promise.resolve(undefined);
    } else if (options.tracer) {
      this.tracer = Promise.resolve(options.tracer);
    } else {
      this.tracer = loadDefaultTracer();
    }
  }

  /** Run `fn` in a span; resolves with its value and the span's trace id. */
  async inSpan<T>(context: StepTelemetryContext, fn: () => Promise<T>): Promise<{ value: T; traceId?: string }> {
    const tracer = await this.tracer;
    if (!tracer) return { value: await fn() };
    return tracer.startActiveSpan(`workflow.step ${context.step}`, async (span) => {
      span.setAttribute('workflow.run_id', context.runId);
      span.setAttribute('workflow.name', context.workflow);
      span.setAttribute('workflow.step', context.step);
      span.setAttribute('workflow.step.sequence', context.sequence);
      span.setAttribute('workflow.step.attempt', context.attempt);
      const traceId = span.spanContext().traceId;
      const recorded = traceId && traceId !== INVALID_TRACE_ID ? traceId : undefined;
      try {
        const value = await fn();
        return { value, traceId: recorded };
      } catch (error) {
        if (error instanceof Error) span.recordException(this.options.redactErrors ? new Error('Workflow step failed') : error);
        span.setStatus({ code: SPAN_STATUS_ERROR, message: this.options.redactErrors ? 'Error' : error instanceof Error ? error.name : 'error' });
        throw Object.assign(new StepAttemptError(error), { traceId: recorded });
      } finally {
        span.end();
      }
    });
  }

  /** Report a failed attempt; returns the tracker's event id, if any. */
  async capture(error: unknown, context: StepTelemetryContext): Promise<string | undefined> {
    const tracker = this.options.errorTracker;
    if (!tracker) return undefined;
    try {
      const id = await tracker.captureException(this.options.redactErrors ? new Error('Workflow step failed') : error, {
        tags: {
          'workflow.name': context.workflow,
          'workflow.step': context.step,
          'workflow.run_id': context.runId,
        },
        extra: { sequence: context.sequence, attempt: context.attempt },
      });
      return typeof id === 'string' && id.length > 0 && id.length <= 128 ? id : undefined;
    } catch {
      // The tracker failing must never fail the step.
      return undefined;
    }
  }
}

/** Carries the trace id of a failed attempt out of its span. */
export class StepAttemptError extends Error {
  traceId?: string;
  constructor(public readonly cause: unknown) {
    super(cause instanceof Error ? cause.message : String(cause));
    this.name = 'StepAttemptError';
  }
}

async function loadDefaultTracer(): Promise<TracerLike | undefined> {
  try {
    const api = (await import('@opentelemetry/api')) as unknown as {
      trace: { getTracer(name: string, version?: string): TracerLike };
    };
    return api.trace.getTracer('@temps-sdk/workflows');
  } catch {
    return undefined;
  }
}
