// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { CronExecutionInfo } from '@/api/client'
import type { StatusTone } from '@temps-sdk/ds'

/** How an execution ended: a `job` or `workflow` run by its `state`, an
 * `http` call by its status code. */
export function executionStatus(execution: CronExecutionInfo): {
  tone: StatusTone
  label: string
} {
  switch (execution.state) {
    case 'running':
      return { tone: 'running', label: 'Running' }
    case 'succeeded':
      return { tone: 'ok', label: 'Success' }
    case 'skipped':
      return { tone: 'idle', label: 'Skipped' }
    case 'failed':
      return { tone: 'error', label: 'Failed' }
    default: {
      const succeeded =
        execution.status_code >= 200 && execution.status_code < 300
      return succeeded
        ? { tone: 'ok', label: 'Success' }
        : { tone: 'error', label: 'Failed' }
    }
  }
}
