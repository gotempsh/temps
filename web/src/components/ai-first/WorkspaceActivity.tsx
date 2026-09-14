// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  CircleAlert,
  CircleCheck,
  Circle,
  Clock3,
  Folder,
  Loader2,
  MessageSquare,
  Square,
  X,
} from 'lucide-react'
import type { WorkspaceHarnessActivity } from '@/api/client'
import { AiHarnessLogo } from '@/components/ui/ai-harness-logo'
import {
  aiHarnessName,
  canonicalHarnessId,
} from '@/components/ui/ai-harness-brand'

const states = [
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

export function WorkspaceActivity({
  projectCount,
  showThreads = true,
  ...activity
}: {
  projectCount?: number
  showThreads?: boolean
  harnesses?: WorkspaceHarnessActivity[]
  loading?: boolean
  error?: boolean
}) {
  return (
    <p
      data-workspace-activity
      className="mt-1.5 flex min-w-0 items-center gap-2 overflow-hidden whitespace-nowrap text-xs tabular-nums sm:text-[0.625rem]"
    >
      {projectCount !== undefined && (
        <span
          className="inline-flex shrink-0 items-center gap-1"
          title={`${projectCount} project${projectCount === 1 ? '' : 's'}`}
          aria-label={`${projectCount} project${projectCount === 1 ? '' : 's'}`}
        >
          <Folder className="size-3.5 shrink-0" aria-hidden="true" />
          <span aria-hidden="true">{projectCount}</span>
        </span>
      )}
      {showThreads && <WorkspaceThreadActivity {...activity} />}
    </p>
  )
}

export function WorkspaceRunningIndicator({
  harnesses,
}: {
  harnesses?: WorkspaceHarnessActivity[]
}) {
  if (!harnesses?.some((harness) => harness.running > 0)) return null
  return (
    <span
      title="Threads running"
      aria-label="Threads running"
      className="inline-flex size-3.5 shrink-0 overflow-hidden text-blue-600 dark:text-blue-400"
    >
      <Loader2
        aria-hidden="true"
        className="size-3.5 animate-spin motion-reduce:animate-none"
      />
    </span>
  )
}

function WorkspaceThreadActivity({
  harnesses,
  loading = false,
  error = false,
}: {
  harnesses?: WorkspaceHarnessActivity[]
  loading?: boolean
  error?: boolean
}) {
  if (error || (!loading && !harnesses)) {
    return (
      <span
        className="flex shrink-0"
        title="Activity unavailable"
        aria-label="Activity unavailable"
      >
        <CircleAlert
          className="size-3.5 shrink-0 text-destructive"
          aria-hidden="true"
        />
      </span>
    )
  }
  if (loading) {
    return (
      <span
        aria-label="Loading workspace activity"
        className="h-4 w-24 shrink-0 animate-pulse rounded bg-muted"
      />
    )
  }
  const grouped = groupHarnessActivity(harnesses ?? [])
  if (!grouped.length) {
    return (
      <span
        className="flex shrink-0 items-center gap-1 text-muted-foreground"
        title="No threads yet"
        aria-label="No threads yet"
      >
        <MessageSquare className="size-3.5 shrink-0" aria-hidden="true" />
        <span aria-hidden="true">0</span>
      </span>
    )
  }
  return (
    <>
      <span className="flex shrink-0 items-center gap-2">
        {grouped.map((harness) => (
          <span
            key={harness.ai_provider}
            className="inline-flex shrink-0 items-center gap-1"
            title={`${aiHarnessName(harness.ai_provider)}: ${harness.total} thread${harness.total === 1 ? '' : 's'}`}
            aria-label={`${aiHarnessName(harness.ai_provider)}: ${harness.total} thread${harness.total === 1 ? '' : 's'}`}
          >
            <AiHarnessLogo providerId={harness.ai_provider} size={14} />
            <span aria-hidden="true">{harness.total}</span>
          </span>
        ))}
      </span>
      <span aria-hidden="true" className="h-3 w-px shrink-0 bg-foreground/15" />
      <span className="flex shrink-0 items-center gap-2">
        {states.map(({ key, label, Icon, className, ...options }) => {
          if (key === 'running') return null
          const count = grouped.reduce((sum, harness) => sum + harness[key], 0)
          if (!count) return null
          return (
            <span
              key={key}
              className={`inline-flex shrink-0 items-center gap-0.5 ${className}`}
              title={`${count} ${label.toLowerCase()} thread${count === 1 ? '' : 's'} · latest turn status`}
              aria-label={`${count} ${label.toLowerCase()} thread${count === 1 ? '' : 's'}`}
            >
              {/* Rotating SVG bounds must not enlarge the row's scroll area. */}
              <span
                aria-hidden="true"
                className="inline-flex size-3 shrink-0 overflow-hidden"
              >
                <Icon
                  className={`size-3 shrink-0 ${'spin' in options && options.spin ? 'animate-spin motion-reduce:animate-none' : ''}`}
                />
              </span>
              <span aria-hidden="true">{count}</span>
            </span>
          )
        })}
      </span>
    </>
  )
}
