// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export const REQUEST_LOG_COLUMNS = [
  { key: 'path', label: 'Path', required: true },
  { key: 'timestamp', label: 'Timestamp' },
  { key: 'method', label: 'Method' },
  { key: 'status_code', label: 'Status' },
  { key: 'routing_status', label: 'Routing' },
  { key: 'response_time_ms', label: 'Response time' },
  { key: 'host', label: 'Host' },
  { key: 'client_ip', label: 'Client IP' },
  { key: 'user_agent', label: 'User agent' },
  { key: 'environment_id', label: 'Environment' },
  { key: 'request_source', label: 'Source' },
  { key: 'cache_status', label: 'Cache' },
] as const

export type RequestLogColumn = (typeof REQUEST_LOG_COLUMNS)[number]['key']

export const DEFAULT_REQUEST_LOG_COLUMNS: RequestLogColumn[] = [
  'path',
  'timestamp',
  'method',
  'status_code',
  'routing_status',
  'response_time_ms',
]

export const REQUEST_LOG_COLUMNS_STORAGE_KEY = 'temps:request-log-columns'
