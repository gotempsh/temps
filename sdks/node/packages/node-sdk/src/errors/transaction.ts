import type { Transaction, Span, SpanContext, TransactionContext, Event, Measurements } from './types.js';

export class TransactionImpl implements Transaction {
  public readonly name: string;
  public readonly op: string;
  public readonly traceId: string;
  public readonly spanId: string;
  public readonly parentSpanId?: string;
  public readonly startTimestamp: number;
  public endTimestamp?: number;
  public status?: Transaction['status'];
  public tags: Record<string, string> = {};
  public data: Record<string, any> = {};
  public sampled?: boolean;
  private children: Span[] = [];
  private finished = false;
  private measurements: Measurements = {};

  constructor(
    context: TransactionContext,
    private onFinish?: (transaction: Transaction) => void
  ) {
    this.name = context.name;
    this.op = context.op;
    this.traceId = context.traceId || this.generateTraceId();
    this.spanId = this.generateSpanId();
    this.parentSpanId = context.parentSpanId;
    this.startTimestamp = Date.now() / 1000;
    this.sampled = context.parentSampled !== false;

    if (context.tags) {
      this.tags = { ...context.tags };
    }
    if (context.data) {
      this.data = { ...context.data };
    }
  }

  finish(): void {
    if (this.finished) {
      return;
    }

    this.endTimestamp = Date.now() / 1000;
    this.finished = true;

    if (this.onFinish) {
      this.onFinish(this);
    }
  }

  setStatus(status: Transaction['status']): void {
    this.status = status;
  }

  setTag(key: string, value: string): void {
    this.tags[key] = value;
  }

  setData(key: string, value: any): void {
    this.data[key] = value;
  }

  setMeasurement(name: string, value: number, unit: string = 'ms'): void {
    this.measurements[name] = { value, unit };
  }

  getMeasurements(): Measurements {
    return { ...this.measurements };
  }

  startChild(spanContext: SpanContext): Span {
    const span = new SpanImpl({
      ...spanContext,
      traceId: this.traceId,
      parentSpanId: this.spanId,
      sampled: this.sampled,
    });

    this.children.push(span);
    return span;
  }

  getSpans(): Span[] {
    return [...this.children];
  }

  toEvent(): Event {
    return {
      type: 'transaction',
      transaction: this.name,
      trace_id: this.traceId,
      span_id: this.spanId,
      parent_span_id: this.parentSpanId,
      start_timestamp: this.startTimestamp,
      timestamp: this.endTimestamp || Date.now() / 1000,
      spans: this.children,
      measurements: this.measurements,
      tags: this.tags,
      extra: this.data,
      contexts: {
        trace: {
          trace_id: this.traceId,
          span_id: this.spanId,
          parent_span_id: this.parentSpanId,
          op: this.op,
          status: this.status,
        },
      },
    };
  }

  private generateTraceId(): string {
    return Array.from({ length: 32 }, () =>
      Math.floor(Math.random() * 16).toString(16)
    ).join('');
  }

  private generateSpanId(): string {
    return Array.from({ length: 16 }, () =>
      Math.floor(Math.random() * 16).toString(16)
    ).join('');
  }
}

export class SpanImpl implements Span {
  public readonly spanId: string;
  public readonly traceId: string;
  public readonly parentSpanId?: string;
  public readonly op: string;
  public readonly description?: string;
  public readonly startTimestamp: number;
  public endTimestamp?: number;
  public status?: Transaction['status'];
  public tags: Record<string, string> = {};
  public data: Record<string, any> = {};
  public sampled?: boolean;
  private children: Span[] = [];
  private finished = false;

  constructor(
    context: SpanContext & {
      traceId: string;
      parentSpanId?: string;
      sampled?: boolean;
    }
  ) {
    this.spanId = this.generateSpanId();
    this.traceId = context.traceId;
    this.parentSpanId = context.parentSpanId;
    this.op = context.op;
    this.description = context.description;
    this.startTimestamp = Date.now() / 1000;
    this.sampled = context.sampled;

    if (context.tags) {
      this.tags = { ...context.tags };
    }
    if (context.data) {
      this.data = { ...context.data };
    }
  }

  finish(): void {
    if (this.finished) {
      return;
    }

    this.endTimestamp = Date.now() / 1000;
    this.finished = true;
  }

  setStatus(status: Transaction['status']): void {
    this.status = status;
  }

  setTag(key: string, value: string): void {
    this.tags[key] = value;
  }

  setData(key: string, value: any): void {
    this.data[key] = value;
  }

  startChild(spanContext: SpanContext): Span {
    const span = new SpanImpl({
      ...spanContext,
      traceId: this.traceId,
      parentSpanId: this.spanId,
      sampled: this.sampled,
    });

    this.children.push(span);
    return span;
  }

  getChildren(): Span[] {
    return [...this.children];
  }

  private generateSpanId(): string {
    return Array.from({ length: 16 }, () =>
      Math.floor(Math.random() * 16).toString(16)
    ).join('');
  }
}
