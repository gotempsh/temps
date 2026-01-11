import { ErrorTrackingClient } from './client.js';
import type { ErrorTrackingOptions, CaptureContext, User, Breadcrumb, TransactionContext, SpanContext, Transaction, Span } from './types.js';

let globalClient: ErrorTrackingClient | null = null;

export function init(options: ErrorTrackingOptions): void {
  globalClient = new ErrorTrackingClient(options);
}

// For testing purposes only - not part of public API
export function __resetForTesting(): void {
  globalClient = null;
}

export function captureException(error: unknown, captureContext?: CaptureContext): string {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return '';
  }
  return globalClient.captureException(error, captureContext);
}

export function captureMessage(
  message: string,
  level?: 'debug' | 'info' | 'warning' | 'error' | 'fatal',
  captureContext?: CaptureContext
): string {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return '';
  }
  return globalClient.captureMessage(message, level, captureContext);
}

export function captureEvent(event: any, captureContext?: CaptureContext): string {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return '';
  }
  return globalClient.captureEvent(event, captureContext);
}

export function setUser(user: User | null): void {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return;
  }
  globalClient.setUser(user);
}

export function setTag(key: string, value: string): void {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return;
  }
  globalClient.setTag(key, value);
}

export function setTags(tags: Record<string, string>): void {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return;
  }
  globalClient.setTags(tags);
}

export function setExtra(key: string, value: any): void {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return;
  }
  globalClient.setExtra(key, value);
}

export function setExtras(extras: Record<string, any>): void {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return;
  }
  globalClient.setExtras(extras);
}

export function setContext(key: string, context: Record<string, any> | null): void {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return;
  }
  globalClient.setContext(key, context);
}

export function addBreadcrumb(breadcrumb: Breadcrumb): void {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return;
  }
  globalClient.addBreadcrumb(breadcrumb);
}

export function clearBreadcrumbs(): void {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return;
  }
  globalClient.clearBreadcrumbs();
}

export function configureScope(callback: (scope: any) => void): void {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return;
  }
  globalClient.configureScope(callback);
}

export function withScope(callback: (scope: any) => void): void {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return;
  }
  globalClient.withScope(callback);
}

export function flush(timeout?: number): Promise<boolean> {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return Promise.resolve(false);
  }
  return globalClient.flush(timeout);
}

export function close(timeout?: number): Promise<boolean> {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return Promise.resolve(false);
  }
  return globalClient.close(timeout);
}

export function getCurrentClient(): ErrorTrackingClient | null {
  return globalClient;
}

export function startTransaction(context: TransactionContext): Transaction {
  if (!globalClient) {
    throw new Error('Error tracking client not initialized. Call init() first.');
  }
  return globalClient.startTransaction(context);
}

export function getCurrentTransaction(): Transaction | null {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return null;
  }
  return globalClient.getCurrentTransaction();
}

export function getLastEventId(): string | null {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return null;
  }
  return globalClient.getLastEventId();
}

export function setLevel(level: 'debug' | 'info' | 'warning' | 'error' | 'fatal'): void {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return;
  }
  globalClient.setLevel(level);
}

export function startSpan(spanContext: SpanContext): Span | undefined {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return undefined;
  }
  return globalClient.startSpan(spanContext);
}

export function captureUserFeedback(feedback: {
  event_id: string;
  name: string;
  email: string;
  comments: string;
}): void {
  if (!globalClient) {
    console.warn('Error tracking client not initialized. Call init() first.');
    return;
  }
  globalClient.captureUserFeedback(feedback);
}

export function isEnabled(): boolean {
  if (!globalClient) {
    return false;
  }
  return globalClient.isEnabled();
}

export { ErrorTrackingClient } from './client.js';
export { Scope } from './scope.js';
export * from './types.js';
