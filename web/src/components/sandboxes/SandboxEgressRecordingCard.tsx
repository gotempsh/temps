// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Recording every outbound connection the sandbox makes, as OpenTelemetry
 * logs of its project. Off by default. When it cannot work here (no
 * project, or the sandbox's node cannot record), the card stays visible in
 * an onboarding state that shows what a record looks like and says exactly
 * what is missing, with a link to where it is fixed.
 */
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Radar } from 'lucide-react'
import { Link } from 'react-router'
import { toast } from 'sonner'

import type { EgressRecordingStatus } from '@/api/client'
import {
  getEgressRecordingOptions,
  getEgressRecordingQueryKey,
  getSandboxQueryKey,
  setEgressRecordingMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { problemDetail } from '@/lib/api-problem'

import { canToggle, EXAMPLE_RECORDS } from './egress-recording'

export function SandboxEgressRecordingCard({
  sandboxId,
  disabled = false,
}: {
  sandboxId: string
  /** Destroyed sandboxes freeze the control. */
  disabled?: boolean
}) {
  const queryClient = useQueryClient()
  const status = useQuery({
    ...getEgressRecordingOptions({ path: { id: sandboxId } }),
    enabled: !disabled,
  })
  const save = useMutation({
    ...setEgressRecordingMutation(),
    meta: { errorTitle: 'Could not change outbound connection recording' },
    onSuccess: (next) => {
      queryClient.setQueryData(
        getEgressRecordingQueryKey({ path: { id: sandboxId } }),
        next
      )
      queryClient.invalidateQueries({
        queryKey: getSandboxQueryKey({ path: { id: sandboxId } }),
      })
      toast.success(
        next.enabled
          ? 'Outbound connections are now recorded'
          : 'Outbound connections are no longer recorded'
      )
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
            A destroyed sandbox makes no connections.
          </p>
        ) : status.isError ? (
          <ErrorAlert
            title="Could not load outbound connection recording"
            description={problemDetail(
              status.error,
              'Whether this sandbox records its connections could not be loaded.'
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
            status={status.data}
            pending={save.isPending}
            onToggle={(enabled) =>
              save.mutate({ path: { id: sandboxId }, body: { enabled } })
            }
          />
        )}
      </CardContent>
    </Card>
  )
}

function RecordingBody({
  status,
  pending,
  onToggle,
}: {
  status: EgressRecordingStatus
  pending: boolean
  onToggle: (enabled: boolean) => void
}) {
  const toggleable = canToggle(status, false) && !pending
  return (
    <>
      <div className="flex items-start justify-between gap-3">
        <label
          htmlFor="egress-recording-switch"
          className="text-sm [overflow-wrap:anywhere]"
        >
          Log every connection this sandbox opens, allowed or refused, as seen
          by the egress proxy
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
              : 'Recording is not available for this sandbox'}
          </p>
          <p className="text-muted-foreground [overflow-wrap:anywhere]">
            {status.reason}
          </p>
          {status.setup_path && (
            <Link
              to={status.setup_path}
              className="inline-block text-xs underline underline-offset-4"
            >
              Open {status.node_name} in Nodes
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
