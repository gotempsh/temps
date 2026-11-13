// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { WorkflowRunStatus } from '@/api/client'
import type { StatusTone } from '@temps-sdk/ds'

export const workflowStatuses: WorkflowRunStatus[] = [
  'queued',
  'running',
  'sleeping',
  'waiting',
  'retrying',
  'completed',
  'failed',
  'cancelled',
  'stuck',
]
export function workflowTone(status: string): StatusTone {
  if (status === 'completed') return 'ok'
  if (status === 'failed' || status === 'stuck') return 'error'
  if (status === 'running') return 'running'
  if (status === 'retrying' || status === 'waiting') return 'warn'
  return 'idle'
}
export function runHref(run: {
  project_id: number
  environment_id: number
  id: string
}): string {
  return `/workflows/${run.project_id}/${run.environment_id}/runs/${encodeURIComponent(run.id)}`
}
