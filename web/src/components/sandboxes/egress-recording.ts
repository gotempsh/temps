// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { EgressRecordingStatus } from '@/api/client'

/** What two records look like, for the onboarding state. */
export const EXAMPLE_RECORDS = [
  'CONNECT api.example.com:443 allowed (tunnel, 1.3 KiB sent, 4.5 KiB received, 230 ms)',
  'CONNECT blocked.example.com:443 refused: no_allow_rule',
]

/** What the switch may do: turning recording off is always allowed. */
export function canToggle(
  status: EgressRecordingStatus,
  disabled: boolean
): boolean {
  if (disabled) return false
  return status.enabled || status.configured
}

/** The badge variant of a connection outcome. */
export function outcomeVariant(
  outcome: string
):
  | 'success-subtle'
  | 'destructive-subtle'
  | 'warning-subtle'
  | 'neutral-subtle' {
  switch (outcome) {
    case 'allowed':
      return 'success-subtle'
    case 'refused':
      return 'destructive-subtle'
    case 'failed':
      return 'warning-subtle'
    default:
      return 'neutral-subtle'
  }
}

/**
 * Where a connection went, as the sandbox asked for it: `CONNECT host:port`
 * for HTTPS, `METHOD host/path` for plain HTTP (never a query string: none
 * is recorded), the relay for a Temps relay.
 */
export function describeDestination(record: {
  kind: string
  host?: string | null
  port?: number | null
  method?: string | null
  path?: string | null
  relay?: string | null
}): string {
  const host = record.host ?? 'unknown host'
  const hostPort = record.port == null ? host : `${host}:${record.port}`
  switch (record.kind) {
    case 'tunnel':
      return `CONNECT ${hostPort}`
    case 'forward': {
      const authority =
        record.port == null || record.port === 80 ? host : hostPort
      return `${record.method ?? 'GET'} ${authority}${record.path ?? '/'}`
    }
    case 'relay':
      return `Temps ${record.relay ?? ''} relay`.replace(/\s+/g, ' ')
    default:
      return hostPort
  }
}

/** `230 ms`, `1.4 s`, `2 min 5 s`. */
export function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms} ms`
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)} s`
  const minutes = Math.floor(ms / 60_000)
  const seconds = Math.round((ms % 60_000) / 1000)
  return seconds === 0 ? `${minutes} min` : `${minutes} min ${seconds} s`
}
