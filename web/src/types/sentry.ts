// Sentry Event Types based on the Sentry protocol

export interface SentrySDK {
  name: string
  version: string
  packages?: Array<{
    name: string
    version: string
  }>
  integrations?: string[]
}

export interface SentryRequest {
  url: string
  method: string
  cookies?: Array<[string, string]>
  headers?: Array<[string, string]>
  query_string?: Array<[string, string]>
}

export interface SentryOS {
  name: string
  type: string
  build?: string
  version: string
  kernel_version?: string
}

export interface SentryApp {
  type: string
  app_memory?: number
  free_memory?: number
  app_start_time?: string
}

export interface SentryOtel {
  type: string
  resource?: {
    'service.name'?: string
    'service.version'?: string
    'service.namespace'?: string
    'telemetry.sdk.name'?: string
    'telemetry.sdk.version'?: string
    'telemetry.sdk.language'?: string
    [key: string]: any
  }
}

export interface SentryTrace {
  op?: string
  type: string
  span_id: string
  trace_id: string
  parent_span_id?: string
  origin?: string
  status?: string
  data?: Record<string, any>
}

export interface SentryDevice {
  arch: string
  type: string
  boot_time?: string
  free_memory?: number
  memory_size?: number
  cpu_description?: string
  processor_count?: number
  processor_frequency?: number
}

export interface SentryCulture {
  type: string
  locale: string
  timezone: string
}

export interface SentryRuntime {
  name: string
  type: string
  version: string
}

export interface SentryResponse {
  type: string
  status_code?: number
}

export interface SentryContexts {
  os?: SentryOS
  app?: SentryApp
  otel?: SentryOtel
  trace?: SentryTrace
  device?: SentryDevice
  culture?: SentryCulture
  runtime?: SentryRuntime
  response?: SentryResponse
  cloud_resource?: { type: string; [key: string]: any }
  [key: string]: any
}

export interface SentryStackFrame {
  colno?: number
  lineno?: number
  filename?: string
  function?: string
  module?: string
  in_app?: boolean
  pre_context?: string[]
  context_line?: string
  post_context?: string[]
  abs_path?: string
  vars?: Record<string, any>
}

export interface SentryException {
  value: string
  type?: string
  module?: string
  mechanism?: {
    type: string
    handled: boolean
    synthetic?: boolean
  }
  stacktrace?: {
    frames: SentryStackFrame[]
  }
}

export interface SentryBreadcrumb {
  timestamp?: number
  category?: string
  level?: string
  message?: string
  data?: Record<string, any>
  type?: string
}

export interface SentryLogEntry {
  formatted: string
  message?: string
  params?: any[]
}

export interface SentrySpan {
  data?: Record<string, any>
  origin?: string
  status?: string
  span_id: string
  trace_id: string
  timestamp?: number
  description?: string
  parent_span_id?: string
  start_timestamp?: number
  op?: string
  tags?: Record<string, any>
}

export interface SentryTransactionInfo {
  source: string
}

export interface SentryUser {
  id?: string
  email?: string
  username?: string
  ip_address?: string
  [key: string]: any
}

export interface SentryEvent {
  sentry: {
    sdk: SentrySDK
    tags?: Array<[string, string]>
    level: string
    type?: string
    spans?: SentrySpan[]
    modules?: Record<string, string>
    release?: string
    request?: SentryRequest
    contexts?: SentryContexts
    event_id: string
    logentry?: SentryLogEntry
    platform: string
    exception?: {
      values: SentryException[]
    }
    timestamp: number
    breadcrumbs?: {
      values: SentryBreadcrumb[]
    }
    environment?: string
    server_name?: string
    transaction?: string
    start_timestamp?: number
    transaction_info?: SentryTransactionInfo
    // Additional fields
    measurements?: Record<string, { value: number; unit?: string }>
    user?: SentryUser
    fingerprint?: string[]
    logger?: string
    extra?: Record<string, any>
    // Performance metrics
    _metrics?: Record<string, any>
    _meta?: Record<string, any>
  }
  source: string
}
