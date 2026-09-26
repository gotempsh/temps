// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { Check, Circle, CircleAlert, Loader2, Terminal } from 'lucide-react'

export type InstallationProgress = {
  id: string
  status: 'running' | 'completed' | 'failed'
  elapsed_ms: number
  stages: Array<{
    stage: string
    message: string
    status: 'running' | 'completed' | 'failed'
    elapsed_ms: number
  }>
}

export function RepositoryInstallProgress({
  progress,
  waitingSeconds,
  failure,
  complete,
  reconnecting,
}: {
  progress?: InstallationProgress
  waitingSeconds: number
  failure?: string
  complete?: boolean
  reconnecting?: boolean
}) {
  const failed = Boolean(failure) || progress?.status === 'failed'
  const finished = complete || progress?.status === 'completed'
  const seconds = Math.floor(
    (progress?.elapsed_ms ?? waitingSeconds * 1000) / 1000
  )
  return (
    <section
      aria-label="Installation progress"
      className="space-y-4 rounded-lg border bg-muted/20 p-4"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 className="flex items-center gap-2 text-sm font-medium">
          <Terminal className="size-4" aria-hidden="true" />
          {failed
            ? 'Installation failed'
            : finished
              ? 'Plugin installed'
              : 'Installing plugin'}
        </h3>
        <span className="text-xs tabular-nums text-muted-foreground">
          {seconds < 60
            ? `${seconds}s`
            : `${Math.floor(seconds / 60)}m ${seconds % 60}s`}
        </span>
      </div>
      <ol
        className="space-y-3 text-sm"
        aria-live="polite"
        aria-relevant="additions text"
      >
        {!progress?.stages.length && (
          <li className="flex items-center gap-2 text-muted-foreground">
            {failed || finished ? (
              <Circle className="size-4" />
            ) : (
              <Loader2 className="size-4 animate-spin motion-reduce:animate-none" />
            )}{' '}
            {finished
              ? 'Installation completed.'
              : failed
                ? 'The host could not complete this request.'
                : 'Waiting for the host to start installation…'}
          </li>
        )}
        {progress?.stages.map((step) => {
          const state =
            step.status === 'running' && failed
              ? 'failed'
              : step.status === 'running' && finished
                ? 'completed'
                : step.status
          const Icon =
            state === 'completed'
              ? Check
              : state === 'failed'
                ? CircleAlert
                : Loader2
          return (
            <li key={step.stage} className="flex items-start gap-2">
              <Icon
                aria-hidden="true"
                className={`mt-0.5 size-4 shrink-0 ${state === 'running' ? 'animate-spin motion-reduce:animate-none' : ''} ${state === 'failed' ? 'text-destructive' : 'text-muted-foreground'}`}
              />
              <span className="min-w-0 flex-1">
                {step.message}
                <span className="sr-only"> — {state}</span>
              </span>
              <span className="text-xs tabular-nums text-muted-foreground">
                {Math.floor(step.elapsed_ms / 1000)}s
              </span>
            </li>
          )
        })}
      </ol>
      {reconnecting && !failed && !finished && (
        <p role="status" className="text-xs text-muted-foreground">
          Progress updates are unavailable. The installation request is still
          running; waiting to reconnect…
        </p>
      )}
      {failure && (
        <p role="alert" className="text-sm text-destructive">
          {failure}
        </p>
      )}
      {!failed && !finished && (
        <p className="text-xs text-muted-foreground">
          The first build can take a few minutes while dependencies and the
          build runtime download.
        </p>
      )}
    </section>
  )
}
