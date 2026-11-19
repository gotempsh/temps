// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import {
  CheckCircle2,
  Circle,
  CircleSlash,
  Loader2,
  XCircle,
} from 'lucide-react'
import type { CreationStep, SandboxCreation } from '@/api/client'
import { Progress } from '@/components/ui/progress'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import {
  creationDurationMs,
  downloadLabel,
  downloadPercent,
  failedStep,
  formatDuration,
  stepDurationMs,
} from './sandbox-creation'

/** The step list's rows while the first poll is in flight. */
const SKELETON_ROWS = 6

/** Ticks once a second while `enabled`, so running steps count up. */
function useTick(enabled: boolean): number {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!enabled) return
    const id = setInterval(() => setNow(Date.now()), 1_000)
    return () => clearInterval(id)
  }, [enabled])
  return now
}

interface SandboxCreationStepsProps {
  /** `undefined` while the first poll is in flight. */
  creation: SandboxCreation | undefined
  className?: string
}

/**
 * What a sandbox create is doing: its ordered steps, each with its status
 * and how long it took (or has taken so far), the image download's
 * progress, and on failure the step that failed and why.
 */
export function SandboxCreationSteps({
  creation,
  className,
}: SandboxCreationStepsProps) {
  const running = creation?.status === 'running'
  const now = useTick(running)

  if (!creation) {
    return (
      <ol
        className={cn('space-y-2.5', className)}
        aria-busy="true"
        aria-label="Sandbox creation steps"
      >
        {Array.from({ length: SKELETON_ROWS }).map((_, i) => (
          <li key={i} className="flex items-center gap-2.5">
            <Skeleton className="size-4 rounded-full" />
            <Skeleton className="h-4 flex-1" />
            <Skeleton className="h-4 w-10" />
          </li>
        ))}
      </ol>
    )
  }

  const failed = failedStep(creation)
  return (
    <div className={cn('space-y-3', className)}>
      <ol className="space-y-2.5" aria-label="Sandbox creation steps">
        {creation.steps.map((step) => (
          <StepRow key={step.kind} step={step} now={now} />
        ))}
      </ol>
      <p
        className="text-xs text-muted-foreground tabular-nums"
        aria-live="polite"
      >
        <CreationSummary creation={creation} failed={failed} now={now} />
      </p>
    </div>
  )
}

function CreationSummary({
  creation,
  failed,
  now,
}: {
  creation: SandboxCreation
  failed: CreationStep | null
  now: number
}) {
  const total = formatDuration(creationDurationMs(creation, now))
  if (creation.status === 'succeeded') return <>Ready in {total}.</>
  if (creation.status === 'failed') {
    return (
      <>
        Failed at “{failed?.title ?? 'an unknown step'}” after {total}.
      </>
    )
  }
  return <>Running for {total}.</>
}

function StepIcon({ status }: { status: CreationStep['status'] }) {
  switch (status) {
    case 'done':
      return (
        <CheckCircle2
          className="size-4 shrink-0 text-emerald-600 dark:text-emerald-500"
          aria-hidden
        />
      )
    case 'running':
      return (
        <Loader2
          className="size-4 shrink-0 animate-spin text-primary motion-reduce:animate-none"
          aria-hidden
        />
      )
    case 'failed':
      return (
        <XCircle className="size-4 shrink-0 text-destructive" aria-hidden />
      )
    case 'skipped':
      return (
        <CircleSlash
          className="size-4 shrink-0 text-muted-foreground/60"
          aria-hidden
        />
      )
    case 'pending':
      return (
        <Circle
          className="size-4 shrink-0 text-muted-foreground/40"
          aria-hidden
        />
      )
  }
}

const STATUS_LABEL: Record<CreationStep['status'], string> = {
  pending: 'pending',
  running: 'running',
  done: 'done',
  failed: 'failed',
  skipped: 'skipped',
}

function StepRow({ step, now }: { step: CreationStep; now: number }) {
  const duration = stepDurationMs(step, now)
  const percent = downloadPercent(step.bytes_done, step.bytes_total)
  const bytes = downloadLabel(step.bytes_done, step.bytes_total)
  return (
    <li className="space-y-1" data-step={step.kind} data-status={step.status}>
      <div className="flex items-start gap-2.5 text-sm">
        <span className="mt-0.5">
          <StepIcon status={step.status} />
        </span>
        <span
          className={cn(
            'min-w-0 flex-1 [overflow-wrap:anywhere]',
            step.status === 'pending' && 'text-muted-foreground',
            step.status === 'skipped' &&
              'text-muted-foreground line-through decoration-muted-foreground/40'
          )}
        >
          {step.title}
          <span className="sr-only">: {STATUS_LABEL[step.status]}</span>
          {step.detail && step.status !== 'failed' && (
            <span className="ml-1.5 text-xs text-muted-foreground">
              {step.detail}
            </span>
          )}
        </span>
        {duration !== null && step.status !== 'skipped' && (
          <span className="shrink-0 text-xs text-muted-foreground tabular-nums">
            {formatDuration(duration)}
          </span>
        )}
      </div>
      {step.status === 'running' && bytes && (
        <div className="ml-6.5 space-y-1">
          {percent !== null && (
            <Progress
              value={percent}
              className="h-1.5"
              aria-label={`${step.title} progress`}
            />
          )}
          <p className="text-xs text-muted-foreground tabular-nums">{bytes}</p>
        </div>
      )}
      {step.status === 'failed' && step.error && (
        <p className="ml-6.5 rounded-md bg-destructive/10 px-2 py-1.5 text-xs text-destructive [overflow-wrap:anywhere]">
          {step.error}
        </p>
      )}
    </li>
  )
}
