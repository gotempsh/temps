// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import {
  type UseQueryResult,
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'
import { Callout } from '@temps-sdk/ds'
import { RotateCcw } from 'lucide-react'
import { toast } from 'sonner'
import type {
  WorkflowRevealTarget,
  WorkflowRunResponse,
  WorkflowStepResponse,
} from '@/api/client'
import { retryWorkflowRun, revealWorkflowPayload } from '@/api/client/sdk.gen'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { Skeleton } from '@/components/ui/skeleton'
import { problemDetail } from '@/lib/api-problem'
import {
  failureHint,
  failureTitle,
  payloadPreview,
  payloadQueryKey,
  payloadText,
  problemStatus,
} from './run-failure'

export function RunFailure({
  path,
  run,
  failedStep,
  invalidate,
}: {
  path: { project_id: number; environment_id: number; run_id: string }
  run: WorkflowRunResponse
  failedStep?: WorkflowStepResponse
  invalidate: readonly (readonly unknown[])[]
}) {
  const cache = useQueryClient()
  const retry = useMutation({
    mutationFn: async () =>
      (await retryWorkflowRun({ path, body: {}, throwOnError: true })).data,
    onSuccess: async () => {
      toast.success('Run queued again')
      await Promise.all(
        invalidate.map((queryKey) => cache.invalidateQueries({ queryKey }))
      )
    },
    onError: (error) =>
      toast.error(problemDetail(error, 'Could not retry the run.')),
  })
  const stepError = failedStep?.has_error ? failedStep : undefined
  const hint = failureHint(run)
  return (
    <Callout tone="error" title={failureTitle(run, failedStep)}>
      <div className="space-y-3">
        {hint && <p>{hint}</p>}
        {(stepError || run.has_error) && (
          <Payload
            path={path}
            label="Error"
            target={stepError ? 'step_error' : 'error'}
            sequence={stepError?.sequence}
          />
        )}
        {run.has_input && <Payload path={path} label="Input" target="input" />}
        <div>
          <Button
            variant="outline"
            size="sm"
            aria-disabled={retry.isPending}
            onClick={() => {
              if (!retry.isPending) retry.mutate()
            }}
          >
            <RotateCcw className="size-4" />
            {retry.isPending ? 'Retrying…' : 'Retry run'}
          </Button>
        </div>
      </div>
    </Callout>
  )
}

/** One encrypted payload, read when the panel opens and shown in place. */
function Payload({
  path,
  label,
  target,
  sequence,
}: {
  path: { project_id: number; environment_id: number; run_id: string }
  label: string
  target: WorkflowRevealTarget
  sequence?: number
}) {
  const payload = useQuery({
    queryKey: payloadQueryKey(path.run_id, target, sequence),
    queryFn: async () =>
      (
        await revealWorkflowPayload({
          path,
          body: { target, sequence: sequence ?? null },
          throwOnError: true,
        })
      ).data,
    staleTime: Infinity,
    retry: false,
  })
  return (
    <section className="space-y-1" aria-label={label}>
      <div className="flex min-h-7 items-center justify-between gap-2">
        <h3 className="text-xs font-medium text-foreground">{label}</h3>
        {payload.isSuccess && (
          <CopyButton
            value={payloadText(payload.data.value)}
            label={`Copy ${label.toLowerCase()}`}
            className="size-7"
          />
        )}
      </div>
      <PayloadBody payload={payload} />
    </section>
  )
}

function PayloadBody({
  payload,
}: {
  payload: UseQueryResult<{ value?: unknown }, unknown>
}) {
  const [expanded, setExpanded] = useState(false)
  if (payload.isPending) return <Skeleton className="h-9 w-full" />
  if (payload.isError)
    return (
      <p role="alert" className="text-xs text-destructive">
        {problemStatus(payload.error) === 403
          ? 'Reading payloads needs the workflow payload read permission on this project.'
          : problemDetail(payload.error, 'Could not read it.')}
      </p>
    )
  const text = payloadText(payload.data.value)
  const preview = payloadPreview(text)
  return (
    <div className="rounded-md border bg-muted/40">
      <pre className="whitespace-pre-wrap px-3 py-2 font-mono text-xs leading-5 text-foreground [overflow-wrap:anywhere]">
        {expanded || preview === null ? text : preview}
      </pre>
      {preview !== null && (
        <button
          type="button"
          className="w-full border-t px-3 py-1.5 text-left text-xs text-muted-foreground hover:text-foreground"
          aria-expanded={expanded}
          onClick={() => setExpanded((open) => !open)}
        >
          {expanded
            ? 'Show less'
            : `Show all (${text.split('\n').length} lines)`}
        </button>
      )}
    </div>
  )
}
