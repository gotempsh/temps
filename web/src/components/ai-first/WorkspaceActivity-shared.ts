// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { CircleCheck, Circle, Clock3, Loader2, Square, X } from 'lucide-react'
import type { WorkspaceHarnessActivity } from '@/api/client'
import {
  aiHarnessName,
  canonicalHarnessId,
} from '@/components/ui/ai-harness-brand'

export const states = [
  {
    key: 'running',
    label: 'Running',
    Icon: Loader2,
    className: 'text-blue-600 dark:text-blue-400',
    spin: true,
  },
  {
    key: 'pending',
    label: 'Pending',
    Icon: Clock3,
    className: 'text-amber-600 dark:text-amber-400',
  },
  {
    key: 'completed',
    label: 'Finished',
    Icon: CircleCheck,
    className: 'text-emerald-600 dark:text-emerald-400',
  },
  { key: 'failed', label: 'Failed', Icon: X, className: 'text-destructive' },
  {
    key: 'cancelled',
    label: 'Stopped',
    Icon: Square,
    className: 'text-muted-foreground',
  },
  {
    key: 'idle',
    label: 'Idle',
    Icon: Circle,
    className: 'text-muted-foreground',
  },
] as const

/** Merge historical harness aliases without losing counts or terminal states. */
export function groupHarnessActivity(harnesses: WorkspaceHarnessActivity[]) {
  const grouped = new Map<string, WorkspaceHarnessActivity>()
  for (const harness of harnesses) {
    const id = canonicalHarnessId(harness.ai_provider)
    const current = grouped.get(id)
    if (!current) {
      grouped.set(id, { ...harness, ai_provider: id })
      continue
    }
    current.total += harness.total
    for (const { key } of states) current[key] += harness[key]
  }
  return [...grouped.values()].sort((a, b) =>
    aiHarnessName(a.ai_provider).localeCompare(aiHarnessName(b.ai_provider))
  )
}
