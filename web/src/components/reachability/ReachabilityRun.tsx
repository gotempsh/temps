// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// A *Make it reachable* run (ADR 064 D4, ADR 069 D4–D5): live progress,
// what it created, conflicts to decide, resume and undo. The run lives on
// the server; this only watches it, so leaving the page loses nothing and
// `?run=<id>` brings it back.

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertTriangle,
  CheckCircle2,
  Circle,
  CircleDot,
  ExternalLink,
  MinusCircle,
  XCircle,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import type { Decision, RunStep, RunView } from '@/api/client'
import {
  decideReachabilityConflictMutation,
  getEnvironmentReachabilityQueryKey,
  getReachabilityChoicesQueryKey,
  getReachabilityRunOptions,
  listReachabilityRunsQueryKey,
  resumeReachabilityRunMutation,
  undoReachabilityRunMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { CloudflareTokenCheck } from '@/components/domains/CloudflareTokenGuide'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { problemDetail } from '@/lib/api-problem'
import {
  addressUrl,
  canResumeRun,
  canUndoRun,
  conflictChoices,
  conflictSubject,
  conflictsToDecide,
  isActiveRun,
  resumeNeedsTailscaleCredential,
  resumeNeedsToken,
  TAILSCALE_RESUME_FIELDS,
} from '@/lib/reachability'
import { RUN_STATE_LABELS, runPollInterval } from '@/lib/reachability-page'
import { cn } from '@/lib/utils'
import { toast } from 'sonner'

export function ReachabilityRun({
  projectId,
  environmentId,
  runId,
  onStartOver,
}: {
  projectId: number
  environmentId: number
  runId: number
  onStartOver: () => void
}) {
  const queryClient = useQueryClient()
  const path = { project_id: projectId, environment_id: environmentId }
  const runPath = { ...path, run_id: runId }
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  const run = useQuery({
    ...getReachabilityRunOptions({ path: runPath }),
    // A run that failed to load (a stale or foreign `?run=`) is not polled;
    // Retry asks again.
    refetchInterval: (query) =>
      runPollInterval(query.state.data?.state, query.state.status),
    retry: false,
  })
  const runState = run.data?.state
  useEffect(() => {
    // Once the run settles, what reaches the environment has changed.
    if (!runState || isActiveRun(runState)) return
    for (const queryKey of [
      getEnvironmentReachabilityQueryKey({ path }),
      getReachabilityChoicesQueryKey({ path }),
      listReachabilityRunsQueryKey({ path }),
    ])
      void queryClient.invalidateQueries({ queryKey })
    // `path` is rebuilt every render; its ids are the dependency.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runState, queryClient, projectId, environmentId])

  const show = (view: RunView) =>
    queryClient.setQueryData(
      getReachabilityRunOptions({ path: runPath }).queryKey,
      view
    )
  const resume = useMutation({
    ...resumeReachabilityRunMutation(),
    onSuccess: show,
  })
  const undo = useMutation({
    ...undoReachabilityRunMutation(),
    onSuccess: (undone) => {
      show(undone)
      if (undone.state === 'undone')
        toast.success('Undone: everything it created is removed')
    },
    onError: (error, variables) => {
      handleSensitiveActionError(error, () => undo.mutate(variables))
    },
  })
  const decide = useMutation({
    ...decideReachabilityConflictMutation(),
    onSuccess: show,
    onError: (error, variables) => {
      handleSensitiveActionError(error, () => decide.mutate(variables))
    },
  })

  return (
    <>
      <RunProgress
        run={run.data}
        loadError={
          run.isError
            ? problemDetail(
                run.error,
                `Could not load run ${runId}. It may belong to another environment, or be gone.`
              )
            : null
        }
        onRetryLoad={() => void run.refetch()}
        undoing={undo.isPending}
        undoError={
          undo.isError
            ? problemDetail(
                undo.error,
                'Undo failed. Try again; nothing else was changed.'
              )
            : null
        }
        onUndo={() => undo.mutate({ path: runPath })}
        resuming={resume.isPending}
        resumeError={
          resume.isError
            ? problemDetail(
                resume.error,
                'Could not resume. Try again; nothing else was changed.'
              )
            : null
        }
        onResume={(answers) =>
          resume.mutate({ path: runPath, body: { answers } })
        }
        deciding={decide.isPending}
        decideError={
          decide.isError
            ? problemDetail(
                decide.error,
                'Could not save your decision. Try again; nothing was changed.'
              )
            : null
        }
        onDecide={(key, decision) =>
          decide.mutate({ path: runPath, body: { key, decision } })
        }
        onStartOver={onStartOver}
      />
      {verificationDialog}
    </>
  )
}

const stepIcons = {
  pending: Circle,
  running: CircleDot,
  done: CheckCircle2,
  failed: XCircle,
  skipped: MinusCircle,
} as const

const stepTone = {
  pending: 'text-muted-foreground',
  running: 'text-primary motion-safe:animate-pulse',
  done: 'text-emerald-700 dark:text-emerald-400',
  failed: 'text-destructive',
  skipped: 'text-muted-foreground',
} as const

function RunProgress({
  run,
  loadError,
  onRetryLoad,
  undoing,
  undoError,
  onUndo,
  resuming,
  resumeError,
  onResume,
  deciding,
  decideError,
  onDecide,
  onStartOver,
}: {
  run: RunView | undefined
  loadError: string | null
  onRetryLoad: () => void
  undoing: boolean
  undoError: string | null
  onUndo: () => void
  resuming: boolean
  resumeError: string | null
  onResume: (answers: Record<string, string>) => void
  deciding: boolean
  decideError: string | null
  onDecide: (key: string, decision: Decision) => void
  onStartOver: () => void
}) {
  // The token, when resuming needs it again; it is never stored before the
  // run saves it.
  const [token, setToken] = useState('')
  // The same for a pasted Tailscale OAuth client (ADR 065 D2).
  const [tailscale, setTailscale] = useState<Record<string, string>>({})
  if (loadError)
    return (
      <Alert variant="destructive">
        <AlertDescription className="flex flex-wrap items-center gap-2">
          <span>{loadError}</span>
          <Button size="sm" variant="outline" onClick={onRetryLoad}>
            Retry
          </Button>
          <Button size="sm" variant="ghost" onClick={onStartOver}>
            Start a new setup
          </Button>
        </AlertDescription>
      </Alert>
    )
  if (!run)
    return (
      <div className="space-y-2" aria-busy="true">
        <Skeleton className="h-5 w-2/3" />
        <Skeleton className="h-5 w-1/2" />
        <Skeleton className="h-5 w-3/5" />
      </div>
    )
  const undone = run.undo.filter((u) => u.ok)
  const undoFailed = run.undo.filter((u) => !u.ok)
  const resumable = canResumeRun(run)
  const needsToken = resumable && resumeNeedsToken(run)
  const needsTailscale = resumable && resumeNeedsTailscaleCredential(run)
  const tailscaleMissing =
    needsTailscale &&
    TAILSCALE_RESUME_FIELDS.some(
      (f) => f.required && !(tailscale[f.key] ?? '').trim()
    )
  const resumeAnswers = (): Record<string, string> => {
    const answers: Record<string, string> = {}
    if (needsToken) answers.cloudflare_api_token = token.trim()
    if (needsTailscale)
      for (const f of TAILSCALE_RESUME_FIELDS) {
        const value = (tailscale[f.key] ?? '').trim()
        if (value) answers[f.key] = value
      }
    return answers
  }
  const conflicts = conflictsToDecide(run)
  const undoPaused = run.state === 'undo_paused'
  return (
    <div className="space-y-4">
      <p className="text-sm">
        <span className="font-medium">{RUN_STATE_LABELS[run.state]}</span>
        {run.address && (
          <span className="text-muted-foreground"> · {run.address}</span>
        )}
        {isActiveRun(run.state) && (
          <span className="text-muted-foreground">
            {' '}
            · You can leave this page; the setup carries on and this link brings
            you back.
          </span>
        )}
      </p>
      {run.state === 'recovering' && (
        <Alert variant="warning">
          <AlertTriangle className="h-4 w-4" />
          <AlertDescription className="space-y-1">
            <p className="font-medium">Recovering an interrupted setup</p>
            <p>
              {run.error ??
                'Temps restarted during this setup and is checking what its unfinished steps made. Resume and undo come back once it has.'}
            </p>
          </AlertDescription>
        </Alert>
      )}
      <ol className="space-y-2" aria-label="Progress">
        {run.steps.map((step: RunStep, index) => {
          const Icon = stepIcons[step.status] ?? Circle
          return (
            <li key={`${step.kind}-${index}`} className="flex gap-2 text-sm">
              <Icon
                aria-hidden="true"
                className={cn(
                  'mt-0.5 size-4 shrink-0',
                  stepTone[step.status] ?? 'text-muted-foreground'
                )}
              />
              <div className="min-w-0">
                <p className="break-words">
                  {step.label}
                  <span className="sr-only"> ({step.status})</span>
                </p>
                {step.detail && (
                  <p className="break-words text-xs text-muted-foreground">
                    {step.detail}
                  </p>
                )}
              </div>
            </li>
          )
        })}
      </ol>
      {run.state === 'succeeded' && run.live_url && (
        <Alert>
          <CheckCircle2 className="h-4 w-4" />
          <AlertDescription className="flex flex-wrap items-center gap-2">
            Live at
            <a
              href={addressUrl(run.live_url)}
              target="_blank"
              rel="noreferrer"
              className="inline-flex items-center gap-1 break-all font-medium underline"
            >
              {run.live_url}
              <ExternalLink aria-hidden="true" className="h-3 w-3" />
            </a>
            <CopyButton value={addressUrl(run.live_url)} minimal />
          </AlertDescription>
        </Alert>
      )}
      {run.error && run.state !== 'undone' && run.state !== 'recovering' && (
        <Alert
          variant={
            run.state === 'waiting' || run.state === 'interrupted'
              ? 'warning'
              : 'destructive'
          }
        >
          <AlertTriangle className="h-4 w-4" />
          <AlertDescription>{run.error}</AlertDescription>
        </Alert>
      )}
      {run.state === 'waiting' && !run.error && (
        <p className="text-sm text-muted-foreground">
          Everything is set up; Temps keeps checking until the address answers.
        </p>
      )}
      {undone.length > 0 && (
        <div className="text-sm">
          <p className="font-medium">
            {run.state === 'failed' ? 'Removed again' : 'Undone'}
          </p>
          <ul className="list-disc pl-5 text-muted-foreground">
            {undone.map((u) => (
              <li key={`${u.label}-${u.resource.type}`}>{u.label}</li>
            ))}
          </ul>
        </div>
      )}
      {undoFailed.length > 0 && (
        <Alert variant="destructive">
          <AlertDescription>
            <p>Could not remove:</p>
            <ul className="list-disc pl-5">
              {undoFailed.map((u) => (
                <li key={`${u.label}-${u.resource.type}`}>
                  {u.label}: {u.error}
                </li>
              ))}
            </ul>
          </AlertDescription>
        </Alert>
      )}
      {undoError && (
        <Alert variant="destructive">
          <AlertDescription>{undoError}</AlertDescription>
        </Alert>
      )}
      {conflicts.length > 0 && (
        <div className="space-y-3 border-t pt-4">
          <p className="text-sm font-medium">
            {undoPaused
              ? 'Undo is waiting for you'
              : 'Someone else changed something this setup works on'}
          </p>
          {conflicts.map((entry) => (
            <div key={entry.key} className="space-y-2 text-sm">
              <p>
                {conflictSubject(entry)}
                {entry.intent.type === 'connector'
                  ? ' also publishes '
                  : ' is now '}
                <span className="font-medium">
                  {entry.conflict ?? 'something else'}
                </span>
                .{' '}
                <span className="text-muted-foreground">
                  Temps leaves it as it is until you decide.
                </span>
              </p>
              <div className="flex flex-wrap gap-2">
                {conflictChoices(entry, undoPaused).map((choice) => (
                  <Button
                    key={choice.decision}
                    size="sm"
                    variant={
                      choice.decision === 'keep' ? 'outline' : 'secondary'
                    }
                    disabled={deciding}
                    onClick={() => onDecide(entry.key, choice.decision)}
                  >
                    {choice.label}
                  </Button>
                ))}
              </div>
            </div>
          ))}
        </div>
      )}
      {decideError && (
        <Alert variant="destructive">
          <AlertDescription>{decideError}</AlertDescription>
        </Alert>
      )}
      {needsToken && (
        <div className="space-y-2">
          <label className="text-sm font-medium" htmlFor="resume-token">
            Cloudflare API token
          </label>
          <Input
            id="resume-token"
            type="password"
            autoComplete="off"
            value={token}
            onChange={(event) => setToken(event.target.value)}
          />
          <p className="text-xs text-muted-foreground">
            Paste it again: Temps never stores it until the run saves the
            account.
          </p>
          <CloudflareTokenCheck token={token} hostname={run.address ?? ''} />
        </div>
      )}
      {needsTailscale && (
        <div className="space-y-2">
          {TAILSCALE_RESUME_FIELDS.map((f) => (
            <div key={f.key} className="space-y-1">
              <label
                className="text-sm font-medium"
                htmlFor={`resume-${f.key}`}
              >
                {f.label}
              </label>
              <Input
                id={`resume-${f.key}`}
                type={f.secret ? 'password' : 'text'}
                autoComplete="off"
                placeholder={f.placeholder}
                value={tailscale[f.key] ?? ''}
                onChange={(event) =>
                  setTailscale((current) => ({
                    ...current,
                    [f.key]: event.target.value,
                  }))
                }
              />
            </div>
          ))}
          <p className="text-xs text-muted-foreground">
            Paste the OAuth client again: Temps never stores it until the run
            saves it.
          </p>
        </div>
      )}
      {resumeError && (
        <Alert variant="destructive">
          <AlertDescription>{resumeError}</AlertDescription>
        </Alert>
      )}
      <div className="flex flex-wrap gap-2">
        {resumable && (
          <Button
            onClick={() => onResume(resumeAnswers())}
            disabled={
              resuming || (needsToken && !token.trim()) || tailscaleMissing
            }
          >
            Resume
          </Button>
        )}
        {canUndoRun(run) && (
          <Button variant="outline" onClick={onUndo} disabled={undoing}>
            Undo
          </Button>
        )}
        {!isActiveRun(run.state) && (
          <Button variant="ghost" onClick={onStartOver}>
            {run.state === 'succeeded'
              ? 'Make another address reachable'
              : 'Start over'}
          </Button>
        )}
      </div>
    </div>
  )
}
