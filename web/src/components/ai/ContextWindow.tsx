// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { CircleHelp } from 'lucide-react'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'

export interface ContextUsage {
  used_tokens: number
  limit_tokens?: number | null
  model?: string | null
  estimated?: boolean
  source: string
  updated_at: string
}

export function contextPercentage(usage?: ContextUsage | null): number | null {
  if (
    !usage ||
    !Number.isSafeInteger(usage.used_tokens) ||
    usage.used_tokens < 0 ||
    !Number.isSafeInteger(usage.limit_tokens) ||
    !usage.limit_tokens ||
    usage.limit_tokens <= 0
  )
    return null
  return Math.round((usage.used_tokens / usage.limit_tokens) * 100)
}

export function ContextWindow({
  usage,
  model,
}: {
  usage?: ContextUsage | null
  model?: string
}) {
  // A selected model can change before its first usage report arrives.
  const current = usage?.model && model && usage.model !== model ? null : usage
  const percentage = contextPercentage(current)
  const used =
    current &&
    Number.isSafeInteger(current.used_tokens) &&
    current.used_tokens >= 0
      ? current.used_tokens
      : null
  const label =
    percentage === null
      ? 'Context'
      : `${current?.estimated ? '≈' : ''}${percentage}%`
  return (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          aria-label="Context window"
          className="flex h-8 items-center gap-1.5 rounded-full px-2 text-xs text-muted-foreground hover:bg-muted hover:text-foreground"
        >
          {percentage === null ? (
            <CircleHelp className="size-4" />
          ) : (
            <svg
              viewBox="0 0 20 20"
              className="size-4 -rotate-90"
              aria-hidden="true"
            >
              <circle
                cx="10"
                cy="10"
                r="8"
                fill="none"
                stroke="currentColor"
                strokeWidth="2.5"
                opacity="0.2"
              />
              <circle
                cx="10"
                cy="10"
                r="8"
                fill="none"
                stroke="currentColor"
                strokeWidth="2.5"
                pathLength="100"
                strokeDasharray={`${Math.min(percentage, 100)} 100`}
              />
            </svg>
          )}
          {label}
        </button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-80 space-y-4 p-4">
        <section className="space-y-2" aria-label="Context usage">
          <h3 className="text-sm font-semibold">Context window</h3>
          <p className="text-sm text-muted-foreground">
            {used === null ? (
              'Usage not reported by this harness yet.'
            ) : (
              <>
                {current?.estimated ? 'Estimated: ' : ''}
                {used.toLocaleString('en-US')}
                {percentage !== null
                  ? ` of ${current?.limit_tokens?.toLocaleString('en-US')} tokens (${percentage}%).`
                  : ' tokens; context limit not reported.'}
              </>
            )}
          </p>
          {percentage !== null && (
            <div
              role="progressbar"
              aria-label="Context window usage"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={Math.min(percentage, 100)}
              className="h-1.5 overflow-hidden rounded-full bg-muted"
            >
              <div
                className="h-full bg-foreground"
                style={{ width: `${Math.min(percentage, 100)}%` }}
              />
            </div>
          )}
          {current && (
            <p className="text-xs text-muted-foreground">
              Last harness report:{' '}
              {new Date(current.updated_at).toLocaleTimeString()}. Not
              cumulative billing usage.
            </p>
          )}
        </section>
        <section className="space-y-2 border-t pt-3">
          <h3 className="text-sm font-semibold">Automatic compaction</h3>
          <p className="flex items-center gap-2 text-sm">
            <span className="size-1.5 rounded-full bg-foreground" />
            Harness default
          </p>
          <p className="text-xs leading-relaxed text-muted-foreground">
            Managed by the harness. Compaction controls are not exposed by this
            integration yet.
          </p>
        </section>
      </PopoverContent>
    </Popover>
  )
}
