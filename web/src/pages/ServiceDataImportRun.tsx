// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  cancelDataImportMutation,
  getDataImportAvailabilityOptions,
  getDataImportOptions,
  getDataImportQueryKey,
  getServiceOptions,
  listDataImportsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type { DataImportRunResponse } from '@/api/client/types.gen'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
import { EmptyState } from '@/components/ui/empty-state'
import { ReadFailure } from '@/components/ui/read-failure'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { cn } from '@/lib/utils'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ArrowLeft,
  CheckCircle2,
  Circle,
  CircleSlash,
  Loader2,
  RotateCcw,
  ScrollText,
  XCircle,
} from 'lucide-react'
import { useEffect, useState, type ReactNode } from 'react'
import { Link, useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'
import {
  formatBytes,
  formatDuration,
  importAgainPath,
  isRunActive,
  phaseSteps,
  problemDetail,
  runHeadline,
  statusLabel,
  statusVariant,
  type PhaseStep,
} from './service-data-import/import-state'

/**
 * One data import run: what was copied from where, how far it got, how
 * long it took, who started it, and everything the transfer printed.
 * Polls while the run is in progress.
 */
export function ServiceDataImportRun() {
  const { id, runId } = useParams<{ id: string; runId: string }>()
  const serviceId = Number(id)
  const importId = Number(runId)
  const valid =
    Number.isSafeInteger(serviceId) &&
    serviceId > 0 &&
    Number.isSafeInteger(importId) &&
    importId > 0
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { setBreadcrumbs } = useBreadcrumbs()

  const serviceQuery = useQuery({
    ...getServiceOptions({ path: { id: serviceId } }),
    enabled: valid,
  })
  const availabilityQuery = useQuery({
    ...getDataImportAvailabilityOptions({ path: { id: serviceId } }),
    enabled: valid,
  })
  const runOptions = getDataImportOptions({
    path: { id: serviceId, run_id: importId },
  })
  const runQuery = useQuery({
    ...runOptions,
    enabled: valid,
    refetchInterval: (query) =>
      query.state.data && isRunActive(query.state.data) ? 2000 : false,
  })
  const run = runQuery.data
  const service = serviceQuery.data?.service
  const objectNoun = availabilityQuery.data?.spec?.object_noun ?? 'table'

  usePageTitle(`Import #${importId}`)
  useEffect(() => {
    if (!valid) return
    setBreadcrumbs([
      { label: 'Databases', href: '/storage' },
      {
        label: service?.name ?? `Database ${serviceId}`,
        href: `/storage/${serviceId}`,
      },
      { label: 'Import data', href: `/storage/${serviceId}/import-data` },
      { label: `Import #${importId}` },
    ])
  }, [setBreadcrumbs, service?.name, serviceId, importId, valid])

  const cancel = useMutation({
    ...cancelDataImportMutation(),
    onSuccess: (updated) => {
      queryClient.setQueryData(runOptions.queryKey, updated)
      queryClient.invalidateQueries({
        queryKey: listDataImportsQueryKey({ path: { id: serviceId } }),
      })
      toast.info('Cancelling the import', {
        description: 'The copy is being stopped.',
      })
    },
    onError: (error) => {
      queryClient.invalidateQueries({
        queryKey: getDataImportQueryKey({
          path: { id: serviceId, run_id: importId },
        }),
      })
      toast.error('Could not cancel the import', {
        description: problemDetail(error),
      })
    },
  })

  const actions = (
    <div className="flex gap-2">
      <Button variant="outline" size="sm" asChild>
        <Link to={`/storage/${serviceId}/import-data`}>
          <ArrowLeft className="h-4 w-4 sm:mr-2" />
          <span className="hidden sm:inline">All imports</span>
        </Link>
      </Button>
      {run && isRunActive(run) && (
        <Button
          variant="outline"
          size="sm"
          disabled={run.cancel_requested || cancel.isPending}
          onClick={() =>
            cancel.mutate({ path: { id: serviceId, run_id: importId } })
          }
        >
          {cancel.isPending ? (
            <Loader2 className="h-4 w-4 animate-spin sm:mr-2" />
          ) : (
            <XCircle className="h-4 w-4 sm:mr-2" />
          )}
          <span className="hidden sm:inline">Cancel import</span>
        </Button>
      )}
      {run && !isRunActive(run) && (
        <Button
          size="sm"
          onClick={() =>
            navigate(importAgainPath(serviceId, run.target_database))
          }
        >
          <RotateCcw className="h-4 w-4 sm:mr-2" />
          <span className="hidden sm:inline">Import again</span>
        </Button>
      )}
    </div>
  )

  return (
    <PageContainer>
      <PageHeader
        title={`Import #${importId}`}
        description={
          run ? (
            <>
              Into <code className="text-xs">{run.target_database}</code>
              {service ? (
                <>
                  {' '}
                  of <strong>{service.name}</strong> ({service.service_type})
                </>
              ) : null}
            </>
          ) : undefined
        }
        actions={actions}
      />
      <RunBody
        valid={valid}
        isPending={runQuery.isPending}
        isError={runQuery.isError}
        error={runQuery.error}
        retrying={runQuery.isFetching}
        onRetry={() => void runQuery.refetch()}
        run={run}
        objectNoun={objectNoun}
      />
    </PageContainer>
  )
}

interface RunBodyProps {
  valid: boolean
  isPending: boolean
  isError: boolean
  error: unknown
  retrying: boolean
  onRetry: () => void
  run: DataImportRunResponse | undefined
  objectNoun: string
}

function RunBody({
  valid,
  isPending,
  isError,
  error,
  retrying,
  onRetry,
  run,
  objectNoun,
}: RunBodyProps) {
  if (!valid) {
    return (
      <EmptyState
        icon={ScrollText}
        title="Import not found"
        description="This link does not point at an import."
      />
    )
  }
  if (isError) {
    return (
      <ReadFailure
        resource="import"
        error={error}
        onRetry={onRetry}
        retrying={retrying}
      />
    )
  }
  if (isPending || !run) {
    return (
      <div className="space-y-6">
        <Skeleton className="h-24 w-full" />
        <Skeleton className="h-48 w-full" />
        <Skeleton className="h-40 w-full" />
      </div>
    )
  }
  return (
    <div className="space-y-6">
      <OutcomeCard run={run} objectNoun={objectNoun} />
      <div className="grid grid-cols-1 gap-6 lg:grid-cols-3">
        <Card className="lg:col-span-1">
          <CardHeader>
            <CardTitle>Progress</CardTitle>
          </CardHeader>
          <CardContent>
            <PhaseTimeline steps={phaseSteps(run)} />
          </CardContent>
        </Card>
        <Card className="lg:col-span-2">
          <CardHeader>
            <CardTitle>Details</CardTitle>
          </CardHeader>
          <CardContent>
            <RunDetails run={run} objectNoun={objectNoun} />
          </CardContent>
        </Card>
      </div>
      <Card>
        <CardHeader>
          <CardTitle>Transfer output</CardTitle>
          <CardDescription>
            The last lines printed by the dump and restore tools. Passwords and
            connection strings are removed before anything is stored.
          </CardDescription>
        </CardHeader>
        <CardContent>
          {run.helper_output ? (
            <pre className="max-h-[480px] overflow-auto whitespace-pre-wrap rounded-md bg-muted p-4 font-mono text-xs">
              {run.helper_output}
            </pre>
          ) : (
            <p className="text-sm text-muted-foreground">
              {isRunActive(run)
                ? 'The output is recorded when the copy finishes.'
                : 'The tools printed nothing for this import.'}
            </p>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

function OutcomeCard({
  run,
  objectNoun,
}: {
  run: DataImportRunResponse
  objectNoun: string
}) {
  const headline = runHeadline(run, objectNoun)
  if (run.status === 'failed' || run.status === 'interrupted') {
    return (
      <Alert variant={run.status === 'failed' ? 'destructive' : 'warning'}>
        <XCircle className="h-4 w-4" />
        <AlertTitle>{headline}</AlertTitle>
        {run.error_message && (
          <AlertDescription>{run.error_message}</AlertDescription>
        )}
      </Alert>
    )
  }
  return (
    <Card>
      <CardContent className="flex flex-col gap-3 pt-6 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex items-center gap-3">
          {isRunActive(run) ? (
            <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
          ) : run.status === 'succeeded' ? (
            <CheckCircle2 className="h-5 w-5 text-success" />
          ) : (
            <CircleSlash className="h-5 w-5 text-muted-foreground" />
          )}
          <div>
            <p className="font-medium">{headline}</p>
            {run.error_message && (
              <p className="text-sm text-muted-foreground">
                {run.error_message}
              </p>
            )}
          </div>
        </div>
        <Badge variant={statusVariant(run.status)}>
          {statusLabel(run.status)}
        </Badge>
      </CardContent>
    </Card>
  )
}

function PhaseTimeline({ steps }: { steps: PhaseStep[] }) {
  return (
    <ol className="space-y-4">
      {steps.map((step) => (
        <li key={step.phase} className="flex items-center gap-3">
          <StepIcon state={step.state} />
          <span
            className={cn(
              'text-sm',
              (step.state === 'pending' || step.state === 'skipped') &&
                'text-muted-foreground',
              step.state === 'failed' && 'font-medium text-destructive'
            )}
          >
            {step.label}
          </span>
        </li>
      ))}
    </ol>
  )
}

function StepIcon({ state }: { state: PhaseStep['state'] }) {
  switch (state) {
    case 'done':
      return <CheckCircle2 className="h-4 w-4 text-success" />
    case 'current':
      return <Loader2 className="h-4 w-4 animate-spin text-primary" />
    case 'failed':
      return <XCircle className="h-4 w-4 text-destructive" />
    default:
      return <Circle className="h-4 w-4 text-muted-foreground" />
  }
}

function RunDetails({
  run,
  objectNoun,
}: {
  run: DataImportRunResponse
  objectNoun: string
}) {
  const [now, setNow] = useState(() => Date.now())
  const active = isRunActive(run)
  useEffect(() => {
    if (!active) return
    const timer = window.setInterval(() => setNow(Date.now()), 1000)
    return () => window.clearInterval(timer)
  }, [active])

  const result =
    run.status === 'succeeded'
      ? [
          typeof run.target_object_count === 'number'
            ? `${run.target_object_count} ${run.target_object_count === 1 ? objectNoun : `${objectNoun}s`}`
            : null,
          formatBytes(run.target_size_bytes),
        ]
          .filter(Boolean)
          .join(' · ') || 'Imported'
      : '—'

  return (
    <dl className="grid grid-cols-1 gap-x-8 gap-y-4 sm:grid-cols-2">
      <Detail label="Source">
        <span className="flex items-center gap-2">
          <code className="break-all text-xs">{run.source}</code>
          <CopyButton value={run.source} minimal />
        </span>
      </Detail>
      <Detail label="Source database">
        <code className="text-xs">{run.source_database}</code>
      </Detail>
      <Detail label="Target database">
        <code className="text-xs">{run.target_database}</code>
      </Detail>
      <Detail label="Existing data">
        {run.replace_existing
          ? 'Replaced (dropped and created again first)'
          : 'Kept — the target had to be empty'}
      </Detail>
      <Detail label="Transfer">
        {run.atomic
          ? 'All or nothing (single transaction)'
          : 'Not atomic — a failure can leave part of the data'}
      </Detail>
      <Detail label="Time limit">
        {Math.round(run.timeout_seconds / 60)} minutes
      </Detail>
      <Detail label="Started">
        <TimeAgo date={run.started_at} />
      </Detail>
      <Detail label="Finished">
        {run.finished_at ? <TimeAgo date={run.finished_at} /> : 'Not yet'}
      </Detail>
      <Detail label="Duration">
        {formatDuration(run.started_at, run.finished_at, now)}
        {active ? ' so far' : ''}
      </Detail>
      <Detail label="Started by">
        {run.started_by ? (
          <>
            {run.started_by.name}{' '}
            <span className="text-muted-foreground">
              ({run.started_by.email})
            </span>
          </>
        ) : (
          '—'
        )}
      </Detail>
      <Detail label="Result">{result}</Detail>
    </dl>
  )
}

function Detail({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="min-w-0 space-y-1">
      <dt className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
        {label}
      </dt>
      <dd className="text-sm">{children}</dd>
    </div>
  )
}
