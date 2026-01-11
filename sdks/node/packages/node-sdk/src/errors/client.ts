import type {
  ErrorTrackingOptions,
  Event,
  Breadcrumb,
  User,
  CaptureContext,
  Integration,
  Transport,
  Transaction,
  TransactionContext,
  Span,
  SpanContext,
} from './types.js';
import { Hub } from './hub.js';
import { createTransportFromDsn } from './transport.js';
import { exceptionFromError, extractErrorMessage, isError } from './stacktrace.js';
import { Scope } from './scope.js';
import { TransactionImpl } from './transaction.js';

export class ErrorTrackingClient {
  private options: ErrorTrackingOptions;
  private hub: Hub;
  private transport: Transport;
  private enabled: boolean = true;
  private currentTransaction: Transaction | null = null;
  private lastEventId: string | null = null;

  constructor(options: ErrorTrackingOptions) {
    this.options = {
      environment: 'production',
      sampleRate: 1.0,
      maxBreadcrumbs: 100,
      attachStacktrace: true,
      debug: false,
      ...options,
    };

    this.hub = new Hub(this.options.maxBreadcrumbs);
    this.transport = createTransportFromDsn(this.options.dsn, this.options.debug);

    if (this.options.integrations) {
      this.setupIntegrations();
    }

    this.setupGlobalHandlers();
  }

  private setupIntegrations(): void {
    if (this.options.integrations) {
      for (const integration of this.options.integrations) {
        integration.setupOnce();
      }
    }
  }

  private setupGlobalHandlers(): void {
    if (typeof process !== 'undefined' && process.on) {
      process.on('uncaughtException', (error: Error) => {
        this.captureException(error, {
          level: 'fatal',
          tags: { handled: 'false' },
        });

        process.exit(1);
      });

      process.on('unhandledRejection', (reason: any) => {
        this.captureException(
          reason instanceof Error ? reason : new Error(String(reason)),
          {
            level: 'error',
            tags: { handled: 'false', type: 'unhandledRejection' },
          }
        );
      });
    }
  }

  captureException(error: unknown, captureContext?: CaptureContext): string {
    if (!this.enabled) {
      return '';
    }

    if (this.shouldIgnoreError(error)) {
      return '';
    }

    const eventId = this.generateEventId();
    const event = this.createEventFromException(error, eventId);

    this.processAndSendEvent(event, captureContext);
    this.lastEventId = eventId;

    return eventId;
  }

  captureMessage(message: string, level: Event['level'] = 'info', captureContext?: CaptureContext): string {
    if (!this.enabled) {
      return '';
    }

    const eventId = this.generateEventId();
    const event: Event = {
      event_id: eventId,
      timestamp: Date.now(),
      level,
      message,
      platform: 'node',
      sdk: {
        name: '@temps-sdk/node-sdk',
        version: '1.0.0',
      },
      environment: this.options.environment,
      release: this.options.release,
      server_name: this.options.serverName,
    };

    this.processAndSendEvent(event, captureContext);
    this.lastEventId = eventId;

    return eventId;
  }

  captureEvent(event: Event, captureContext?: CaptureContext): string {
    if (!this.enabled) {
      return '';
    }

    const eventId = event.event_id || this.generateEventId();
    const enrichedEvent: Event = {
      ...event,
      event_id: eventId,
      timestamp: event.timestamp || Date.now(),
      platform: event.platform || 'node',
      sdk: event.sdk || {
        name: '@temps-sdk/node-sdk',
        version: '1.0.0',
      },
      environment: event.environment || this.options.environment,
      release: event.release || this.options.release,
      server_name: event.server_name || this.options.serverName,
    };

    this.processAndSendEvent(enrichedEvent, captureContext);
    this.lastEventId = eventId;

    return eventId;
  }

  private createEventFromException(error: unknown, eventId: string): Event {
    const event: Event = {
      event_id: eventId,
      timestamp: Date.now(),
      level: 'error',
      platform: 'node',
      sdk: {
        name: '@temps-sdk/node-sdk',
        version: '1.0.0',
      },
      environment: this.options.environment,
      release: this.options.release,
      server_name: this.options.serverName,
    };

    if (isError(error)) {
      event.exception = {
        values: [exceptionFromError(error as Error)],
      };
    } else {
      event.message = extractErrorMessage(error);
    }

    return event;
  }

  private processAndSendEvent(event: Event, captureContext?: CaptureContext): void {
    if (Math.random() > (this.options.sampleRate || 1)) {
      return;
    }

    let finalEvent = this.hub.applyToEvent(event, captureContext);

    if (this.options.beforeSend) {
      const processed = this.options.beforeSend(finalEvent);
      if (processed === null) {
        return;
      }
      finalEvent = processed;
    }

    this.transport.sendEvent(finalEvent).catch(error => {
      if (this.options.debug) {
        console.error('Failed to send event:', error);
      }
    });
  }

  private shouldIgnoreError(error: unknown): boolean {
    if (!this.options.ignoreErrors || this.options.ignoreErrors.length === 0) {
      return false;
    }

    const message = extractErrorMessage(error);

    return this.options.ignoreErrors.some(pattern => {
      if (typeof pattern === 'string') {
        return message.includes(pattern);
      }
      return pattern.test(message);
    });
  }

  private generateEventId(): string {
    return Array.from({ length: 32 }, () =>
      Math.floor(Math.random() * 16).toString(16)
    ).join('');
  }

  setUser(user: User | null): void {
    this.hub.setUser(user);
  }

  setTag(key: string, value: string): void {
    this.hub.setTag(key, value);
  }

  setTags(tags: Record<string, string>): void {
    this.hub.setTags(tags);
  }

  setExtra(key: string, value: any): void {
    this.hub.setExtra(key, value);
  }

  setExtras(extras: Record<string, any>): void {
    this.hub.setExtras(extras);
  }

  setContext(key: string, context: Record<string, any> | null): void {
    this.hub.setContext(key, context);
  }

  addBreadcrumb(breadcrumb: Breadcrumb): void {
    this.hub.addBreadcrumb(breadcrumb);
  }

  clearBreadcrumbs(): void {
    this.hub.clearBreadcrumbs();
  }

  configureScope(callback: (scope: Scope) => void): void {
    this.hub.configureScope(callback);
  }

  withScope(callback: (scope: Scope) => void): void {
    this.hub.withScope(callback);
  }

  setEnabled(enabled: boolean): void {
    this.enabled = enabled;
  }

  flush(timeout: number = 2000): Promise<boolean> {
    return new Promise(resolve => {
      setTimeout(() => resolve(true), Math.min(timeout, 100));
    });
  }

  close(timeout: number = 2000): Promise<boolean> {
    this.enabled = false;
    return this.flush(timeout);
  }

  startTransaction(context: TransactionContext): Transaction {
    const transaction = new TransactionImpl(context, (finishedTransaction) => {
      if (this.options.tracesSampleRate && Math.random() <= this.options.tracesSampleRate) {
        const event = (finishedTransaction as TransactionImpl).toEvent();
        this.processAndSendEvent(event);
      }
    });

    this.currentTransaction = transaction;
    return transaction;
  }

  getCurrentTransaction(): Transaction | null {
    return this.currentTransaction;
  }

  getLastEventId(): string | null {
    return this.lastEventId;
  }

  setLevel(level: Event['level']): void {
    this.hub.setLevel(level);
  }

  startSpan(spanContext: SpanContext): Span | undefined {
    if (!this.currentTransaction) {
      return undefined;
    }
    return this.currentTransaction.startChild(spanContext);
  }

  captureUserFeedback(feedback: {
    event_id: string;
    name: string;
    email: string;
    comments: string;
  }): void {
    const event: Event = {
      event_id: this.generateEventId(),
      timestamp: Date.now(),
      type: 'default',
      user: {
        email: feedback.email,
        username: feedback.name,
      },
      extra: {
        feedback: {
          event_id: feedback.event_id,
          comments: feedback.comments,
        },
      },
      tags: {
        source: 'user_feedback',
      },
    };

    this.processAndSendEvent(event);
  }

  isEnabled(): boolean {
    return this.enabled;
  }

  getOptions(): ErrorTrackingOptions {
    return { ...this.options };
  }
}
