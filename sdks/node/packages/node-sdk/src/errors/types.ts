export interface User {
  id?: string;
  username?: string;
  email?: string;
  ip_address?: string;
  [key: string]: any;
}

export interface Breadcrumb {
  timestamp?: number;
  message?: string;
  category?: string;
  level?: 'debug' | 'info' | 'warning' | 'error' | 'critical';
  type?: 'default' | 'http' | 'navigation' | 'console';
  data?: Record<string, any>;
}

export interface StackFrame {
  filename?: string;
  function?: string;
  lineno?: number;
  colno?: number;
  abs_path?: string;
  context_line?: string;
  pre_context?: string[];
  post_context?: string[];
  in_app?: boolean;
  vars?: Record<string, any>;
}

export interface Exception {
  type?: string;
  value?: string;
  module?: string;
  mechanism?: {
    type?: string;
    handled?: boolean;
    data?: Record<string, any>;
  };
  stacktrace?: {
    frames?: StackFrame[];
  };
}

export interface Request {
  url?: string;
  method?: string;
  data?: any;
  query_string?: string;
  cookies?: Record<string, string>;
  headers?: Record<string, string>;
  env?: Record<string, string>;
}

export interface Event {
  event_id?: string;
  timestamp?: number;
  level?: 'debug' | 'info' | 'warning' | 'error' | 'fatal';
  logger?: string;
  platform?: string;
  sdk?: {
    name?: string;
    version?: string;
  };
  release?: string;
  environment?: string;
  server_name?: string;
  message?: string;
  user?: User;
  request?: Request;
  contexts?: Record<string, any>;
  tags?: Record<string, string>;
  extra?: Record<string, any>;
  fingerprint?: string[];
  exception?: {
    values?: Exception[];
  };
  breadcrumbs?: Breadcrumb[];
  type?: 'transaction' | 'error' | 'default';
  transaction?: string;
  spans?: Span[];
  start_timestamp?: number;
  measurements?: Measurements;
  trace_id?: string;
  span_id?: string;
  parent_span_id?: string;
}

export interface ErrorTrackingOptions {
  dsn: string;
  environment?: string;
  release?: string;
  sampleRate?: number;
  maxBreadcrumbs?: number;
  beforeSend?: (event: Event) => Event | null;
  integrations?: Integration[];
  debug?: boolean;
  serverName?: string;
  ignoreErrors?: (string | RegExp)[];
  attachStacktrace?: boolean;
  tracesSampleRate?: number;
}

export interface Integration {
  name: string;
  setupOnce(): void;
}

export interface Transport {
  sendEvent(event: Event): Promise<void>;
}

export interface Scope {
  setUser(user: User | null): void;
  setTag(key: string, value: string): void;
  setTags(tags: Record<string, string>): void;
  setExtra(key: string, value: any): void;
  setExtras(extras: Record<string, any>): void;
  setContext(key: string, context: Record<string, any> | null): void;
  setLevel(level: Event['level']): void;
  addBreadcrumb(breadcrumb: Breadcrumb): void;
  clearBreadcrumbs(): void;
  clear(): void;
}

export interface CaptureContextScope {
  user?: User | null;
  tags?: Record<string, string>;
  extra?: Record<string, any>;
  contexts?: Record<string, any>;
  level?: Event['level'];
}

export type CaptureContext = CaptureContextScope | ((scope: Scope) => void);

export interface Transaction {
  name: string;
  op: string;
  traceId: string;
  spanId: string;
  parentSpanId?: string;
  startTimestamp: number;
  endTimestamp?: number;
  status?: 'ok' | 'cancelled' | 'internal_error' | 'unknown_error' | 'invalid_argument' | 'deadline_exceeded' | 'not_found' | 'already_exists' | 'permission_denied' | 'resource_exhausted' | 'failed_precondition' | 'aborted' | 'out_of_range' | 'unimplemented' | 'unavailable' | 'data_loss' | 'unauthenticated';
  tags?: Record<string, string>;
  data?: Record<string, any>;
  sampled?: boolean;
  finish(): void;
  setStatus(status: Transaction['status']): void;
  setTag(key: string, value: string): void;
  setData(key: string, value: any): void;
  startChild(spanContext: SpanContext): Span;
}

export interface SpanContext {
  op: string;
  description?: string;
  tags?: Record<string, string>;
  data?: Record<string, any>;
}

export interface Span {
  spanId: string;
  traceId: string;
  parentSpanId?: string;
  op: string;
  description?: string;
  startTimestamp: number;
  endTimestamp?: number;
  status?: Transaction['status'];
  tags?: Record<string, string>;
  data?: Record<string, any>;
  sampled?: boolean;
  finish(): void;
  setStatus(status: Transaction['status']): void;
  setTag(key: string, value: string): void;
  setData(key: string, value: any): void;
  startChild(spanContext: SpanContext): Span;
}

export interface TransactionContext {
  name: string;
  op: string;
  tags?: Record<string, string>;
  data?: Record<string, any>;
  parentSampled?: boolean;
  traceId?: string;
  parentSpanId?: string;
}

export interface Measurements {
  fcp?: { value: number; unit: string };
  lcp?: { value: number; unit: string };
  fid?: { value: number; unit: string };
  cls?: { value: number; unit: string };
  ttfb?: { value: number; unit: string };
  [key: string]: { value: number; unit: string } | undefined;
}
