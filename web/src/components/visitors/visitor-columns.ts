// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export const VISITOR_COLUMNS = [
  { key: 'visitor', label: 'Visitor', required: true },
  { key: 'last_seen', label: 'Last seen' },
  { key: 'location', label: 'Location' },
  { key: 'source', label: 'Source' },
  { key: 'browser', label: 'Browser / OS' },
  { key: 'first_seen', label: 'First seen' },
  { key: 'current_page', label: 'Latest page' },
  { key: 'ip_address', label: 'IP address' },
  { key: 'environment', label: 'Environment' },
  { key: 'user_agent', label: 'User agent' },
] as const

export type VisitorColumn = (typeof VISITOR_COLUMNS)[number]['key']
export const DEFAULT_VISITOR_COLUMNS: VisitorColumn[] = [
  'visitor',
  'last_seen',
  'location',
  'source',
  'browser',
]
export const VISITOR_COLUMNS_STORAGE_KEY = 'temps:visitor-columns'
