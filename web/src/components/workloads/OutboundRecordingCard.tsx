// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Recording every outbound connection a workload makes, as OpenTelemetry
 * logs. One card for every kind of workload: the page says which one
 * (`sandboxEgress(id)`, `environmentEgress(projectId, environmentId)`). Off
 * by default. When it cannot work here (no node the workload runs on can
 * record), the card stays visible in an onboarding state that shows what a
 * record looks like and says exactly what is missing, with a link to where
 * it is fixed.
 */
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Radar } from 'lucide-react'
import { Link } from 'react-router'
import { toast } from 'sonner'

import type { EgressRecordingStatus } from '@/api/client'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { problemDetail } from '@/lib/api-problem'

import {
  blockingNodeName,
  canToggle,
  egressRecordsKey,
  egressStatusKey,
  EXAMPLE_RECORDS,
  toggleToast,
  type EgressWorkload,
} from './egress-recording'

export function OutboundRecordingCard({
  workload,
  disabled = false,
  disabledNote,
}: {
  workload: EgressWorkload
  /** A workload that is gone freezes the control. */
  disabled?: boolean
  /** Why it is frozen, e.g. "A destroyed sandbox makes no connections." */
  disabledNote?: string
}) {
  const queryClient = useQueryClient()
  const statusKey = egressStatusKey(workload)
  const status = useQuery({
    queryKey: statusKey,
    queryFn: ({ signal }) => workload.status(signal),
    enabled: !disabled,
  })
  const save = useMutation({
    mutationFn: (enabled: boolean) => workload.setEnabled(enabled),
    meta: { errorTitle: 'Could not change outbound connection recording' },
    onSuccess: (next) => {
      queryClient.setQueryData(statusKey, next)
      queryClient.invalidateQueries({ queryKey: egressRecordsKey(workload) })
      for (const queryKey of workload.invalidates ?? []) {
        queryClient.invalidateQueries({ queryKey })
      }
      const answer = toggleToast(next)
      if (answer.kind === 'warning') {
        toast.warning(answer.title, { description: answer.description })
      } else {
        toast.success(answer.title)
      }
    },
  })

  return (
    <Card>
      <CardHeader className="pb-3">
        <CardTitle className="flex items-center gap-2 text-sm font-medium">
          <Radar className="h-4 w-4" aria-hidden />
          Record outbound connections
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        {disabled ? (
          <p className="text-sm text-muted-foreground">
            {disabledNote ?? `This ${workload.noun} makes no connections.`}
          </p>
        ) : status.isError ? (
          <ErrorAlert
            title="Could not load outbound connection recording"
            description={problemDetail(
              status.error,
              `Whether this ${workload.noun} records its connections could not be loaded.`
            )}
            retry={() => status.refetch()}
            retrying={status.isFetching}
          />
        ) : status.data === undefined ? (
          <div className="space-y-2" aria-busy="true">
            <Skeleton className="h-6 w-2/3" />
            <Skeleton className="h-12 w-full" />
          </div>
        ) : (
          <RecordingBody
            workload={workload}
            status={status.data}
            pending={save.isPending}
            onToggle={(enabled) => save.mutate(enabled)}
          />
        )}
      </CardContent>
    </Card>
  )
}

function RecordingBody({
  workload,
  status,
  pending,
  onToggle,
}: {
  workload: EgressWorkload
  status: EgressRecordingStatus
  pending: boolean
  onToggle: (enabled: boolean) => void
}) {
  const toggleable = canToggle(status, false) && !pending
  // A workload on several nodes records where a node can; say where not.
  const unrecorded = status.configured
    ? status.nodes.filter((node) => !node.recording)
    : []
  return (
    <>
      <div className="flex items-start justify-between gap-3">
        <label
          htmlFor="egress-recording-switch"
          className="text-sm [overflow-wrap:anywhere]"
        >
          Log every connection this {workload.noun} opens, allowed or refused,
          as seen by the egress proxy
        </label>
        <Switch
          id="egress-recording-switch"
          checked={status.enabled}
          disabled={!toggleable}
          onCheckedChange={onToggle}
        />
      </div>

      {!status.configured && (
        <div
          role="status"
          className="space-y-2 rounded-md border border-dashed p-3 text-sm"
        >
          <p className="font-medium">
            {status.enabled
              ? 'Recording is on, but nothing is being recorded'
              : `Recording is not available for this ${workload.noun}`}
          </p>
          <p className="text-muted-foreground [overflow-wrap:anywhere]">
            {status.reason}
          </p>
          {status.setup_path && (
            <Link
              to={status.setup_path}
              className="inline-block text-xs underline underline-offset-4"
            >
              Open {blockingNodeName(status)} in Nodes
            </Link>
          )}
          <p className="text-xs text-muted-foreground">
            Once available, each connection becomes one log record, e.g.
          </p>
          <ul className="space-y-1 font-mono text-xs text-muted-foreground">
            {EXAMPLE_RECORDS.map((line) => (
              <li key={line} className="[overflow-wrap:anywhere]">
                {line}
              </li>
            ))}
          </ul>
        </div>
      )}

      {workload.appliesNote && status.configured && (
        <p className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
          {workload.appliesNote}
        </p>
      )}

      {unrecorded.length > 0 && (
        <ul
          aria-label="Nodes that do not record"
          className="space-y-1 text-xs text-muted-foreground"
        >
          {unrecorded.map((node) => (
            <li key={node.node_id} className="[overflow-wrap:anywhere]">
              Not recorded on {node.node_name}: {node.reason}
            </li>
          ))}
        </ul>
      )}

      {status.configured && (
        <p className="text-xs text-muted-foreground">
          Destination, outcome, bytes and duration only: never headers, bodies
          or query strings.
          {status.enabled && status.logs_path && (
            <>
              {' '}
              <Link
                to={status.logs_path}
                className="underline underline-offset-4"
              >
                Open recorded connections
              </Link>
            </>
          )}
        </p>
      )}
    </>
  )
}
