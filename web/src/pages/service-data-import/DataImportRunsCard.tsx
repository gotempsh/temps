// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  cancelDataImportMutation,
  listDataImportsOptions,
  listDataImportsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type { DataImportRunResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { EmptyState } from '@/components/ui/empty-state'
import { ReadFailure } from '@/components/ui/read-failure'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { RecordLink } from '@temps-sdk/ds'
import { DatabaseZap, Loader2, XCircle } from 'lucide-react'
import { Fragment, useEffect, useRef, useState } from 'react'
import { toast } from 'sonner'
import {
  importPollInterval,
  importRunPath,
  isRunActive,
  newlySettledRuns,
  problemDetail,
  resultSummary,
  runningLabel,
  statusLabel,
  statusVariant,
} from './import-state'

const PAGE_SIZE = 20

interface DataImportRunsCardProps {
  serviceId: number
  /** Singular noun for the engine's data containers ("table"). */
  objectNoun: string
}

export function DataImportRunsCard({
  serviceId,
  objectNoun,
}: DataImportRunsCardProps) {
  const queryClient = useQueryClient()
  const [page, setPage] = useState(1)
  const options = listDataImportsOptions({
    path: { id: serviceId },
    query: { page, page_size: PAGE_SIZE },
  })
  const runsQuery = useQuery({
    ...options,
    refetchInterval: (query) => importPollInterval(query.state.data?.items),
  })
  const runs = runsQuery.data?.items

  // Announce runs that finish while the page is open.
  const previousRuns = useRef<DataImportRunResponse[] | undefined>(undefined)
  useEffect(() => {
    for (const run of newlySettledRuns(previousRuns.current, runs)) {
      if (run.status === 'succeeded') {
        toast.success(`Import into '${run.target_database}' finished`, {
          description: resultSummary(run, objectNoun) ?? undefined,
        })
      } else {
        toast.error(
          `Import into '${run.target_database}' ${statusLabel(run.status).toLowerCase()}`,
          { description: run.error_message?.split('\n')[0] ?? undefined }
        )
      }
    }
    previousRuns.current = runs
  }, [runs, objectNoun])

  const cancel = useMutation({
    ...cancelDataImportMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: listDataImportsQueryKey({ path: { id: serviceId } }),
      })
      toast.info('Cancelling the import', {
        description: 'The copy is being stopped.',
      })
    },
    onError: (error) => {
      toast.error('Could not cancel the import', {
        description: problemDetail(error),
      })
    },
  })

  const total = runsQuery.data?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / PAGE_SIZE))

  return (
    <Card>
      <CardHeader>
        <CardTitle>Import history</CardTitle>
        <CardDescription>
          Imports into this service, newest first. Source credentials are never
          shown.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <RunsBody
          serviceId={serviceId}
          isPending={runsQuery.isPending}
          isError={runsQuery.isError}
          error={runsQuery.error}
          retrying={runsQuery.isFetching}
          onRetry={() => void runsQuery.refetch()}
          runs={runs}
          objectNoun={objectNoun}
          cancellingId={
            cancel.isPending ? cancel.variables?.path.run_id : undefined
          }
          onCancel={(runId) =>
            cancel.mutate({ path: { id: serviceId, run_id: runId } })
          }
        />
        {total > PAGE_SIZE && (
          <ResponsivePagination
            className="mt-4"
            page={page}
            pageSize={PAGE_SIZE}
            total={total}
            totalPages={totalPages}
            onPageChange={setPage}
            ariaLabel="Import history pages"
          />
        )}
      </CardContent>
    </Card>
  )
}

interface RunsBodyProps {
  serviceId: number
  isPending: boolean
  isError: boolean
  error: unknown
  retrying: boolean
  onRetry: () => void
  runs: DataImportRunResponse[] | undefined
  objectNoun: string
  cancellingId: number | undefined
  onCancel: (runId: number) => void
}

function RunsBody({
  serviceId,
  isPending,
  isError,
  error,
  retrying,
  onRetry,
  runs,
  objectNoun,
  cancellingId,
  onCancel,
}: RunsBodyProps) {
  if (isPending) {
    return (
      <div className="space-y-2">
        <Skeleton className="h-10 w-full" />
        <Skeleton className="h-10 w-full" />
        <Skeleton className="h-10 w-full" />
      </div>
    )
  }
  const failure = isError ? (
    <ReadFailure
      resource="import history"
      error={error}
      cached={Boolean(runs && runs.length > 0)}
      onRetry={onRetry}
      retrying={retrying}
      embedded
    />
  ) : null
  // A failed refresh keeps the rows already loaded on screen, with the
  // error above them; only a first load that failed shows the error alone.
  if (isError && (!runs || runs.length === 0)) {
    return failure
  }
  if (!runs || runs.length === 0) {
    return (
      <EmptyState
        icon={DatabaseZap}
        title="No imports yet"
        description="Imports you start above are tracked here, with their result or the reason they failed."
        size="compact"
      />
    )
  }
  return (
    <div className="space-y-4">
      {failure}
      <div className="min-w-0 overflow-x-auto">
        <Table className="min-w-[640px]">
          <TableHeader>
            <TableRow>
              <TableHead>Target</TableHead>
              <TableHead>Started</TableHead>
              <TableHead className="hidden md:table-cell">Source</TableHead>
              <TableHead>Status</TableHead>
              <TableHead>Result</TableHead>
              <TableHead className="w-0" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {runs.map((run) => (
              <Fragment key={run.id}>
                <TableRow>
                  <TableCell className="font-mono text-sm">
                    <RecordLink
                      to={importRunPath(serviceId, run.id)}
                      aria-label={`View import ${run.id} into ${run.target_database}`}
                    >
                      {run.target_database}
                    </RecordLink>
                    {run.replace_existing && (
                      <Badge variant="outline" className="ml-2">
                        replaced
                      </Badge>
                    )}
                  </TableCell>
                  <TableCell className="whitespace-nowrap">
                    <TimeAgo date={run.started_at} />
                  </TableCell>
                  <TableCell className="hidden max-w-[280px] truncate font-mono text-xs text-muted-foreground md:table-cell">
                    {run.source}
                  </TableCell>
                  <TableCell>
                    <Badge variant={statusVariant(run.status)}>
                      {statusLabel(run.status)}
                    </Badge>
                  </TableCell>
                  <TableCell className="text-sm text-muted-foreground">
                    <RunResult run={run} objectNoun={objectNoun} />
                  </TableCell>
                  <TableCell>
                    {isRunActive(run) && (
                      <Button
                        variant="ghost"
                        size="sm"
                        disabled={
                          run.cancel_requested || cancellingId === run.id
                        }
                        aria-label={`Cancel import ${run.id} into ${run.target_database}`}
                        onClick={() => onCancel(run.id)}
                      >
                        <XCircle className="h-4 w-4 sm:mr-2" />
                        <span className="hidden sm:inline">Cancel</span>
                      </Button>
                    )}
                  </TableCell>
                </TableRow>
                {run.error_message && (
                  <TableRow className="hover:bg-transparent">
                    <TableCell colSpan={6} className="pt-0">
                      <p className="mb-1 font-mono text-xs text-muted-foreground md:hidden">
                        {run.source}
                      </p>
                      <p className="line-clamp-2 text-sm text-muted-foreground">
                        {run.error_message}
                      </p>
                    </TableCell>
                  </TableRow>
                )}
              </Fragment>
            ))}
          </TableBody>
        </Table>
      </div>
    </div>
  )
}

function RunResult({
  run,
  objectNoun,
}: {
  run: DataImportRunResponse
  objectNoun: string
}) {
  if (isRunActive(run)) {
    return (
      <span className="inline-flex items-center gap-2">
        <Loader2 className="h-3 w-3 animate-spin" />
        {runningLabel(run)}
      </span>
    )
  }
  if (run.status === 'succeeded') {
    return <>{resultSummary(run, objectNoun) ?? 'Imported'}</>
  }
  return <>—</>
}
