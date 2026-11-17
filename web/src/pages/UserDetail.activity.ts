// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { AuditLogResponse } from '@/api/client/types.gen'

export function summarizeActivity(
  logs: AuditLogResponse[],
  cutoff: number,
  batchSize = 100
) {
  let actions30d = 0
  let lastActivity: number | undefined
  const calls = new Set<string>()
  for (const log of logs) {
    if (lastActivity === undefined || log.audit_date > lastActivity)
      lastActivity = log.audit_date
    if (log.audit_date < cutoff) continue
    const data = log.data
    const callId =
      log.operation_type === 'EXTERNAL_PLUGIN_AI_WRITE' &&
      typeof data === 'object' &&
      data !== null &&
      'call_id' in data &&
      typeof data.call_id === 'string'
        ? data.call_id
        : undefined
    if (callId) {
      if (calls.has(callId)) continue
      calls.add(callId)
    }
    actions30d++
  }
  // A full newest-first batch cannot establish the total when every row is
  // inside the window, regardless of how many calls deduplication removes.
  const actionsIncomplete =
    logs.length >= batchSize && logs.every((log) => log.audit_date >= cutoff)
  return { actions30d, lastActivity, actionsIncomplete }
}
