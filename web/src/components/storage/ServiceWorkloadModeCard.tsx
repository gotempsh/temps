// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
import {
  useMutation,
  useQuery,
  useQueryClient,
  type UseQueryResult,
} from '@tanstack/react-query'
import { Callout } from '@temps-sdk/ds'
import { Layers, Loader2, RefreshCw, RotateCcw } from 'lucide-react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { ConfirmDialog } from '@/components/ui/confirm-dialog'
import { SettingsSection } from '@/components/ui/settings-section'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useIsInstanceAdmin } from '@/hooks/useIsInstanceAdmin'
import { problemDetail } from '@/lib/api-problem'
import { isPermissionDenied } from '@/lib/environment-workload'
import { httpStatusOf } from '@/lib/http-error-status'
import {
  getServiceWorkloadMode,
  getServiceWorkloadPendingChanges,
  recreateAvailability,
  recreateServiceFromWorkloadSpec,
  serviceLocation,
  serviceModeChangeConsequence,
  serviceWorkloadModeQueryKey,
  serviceWorkloadModeSummary,
  serviceWorkloadPendingQueryKey,
  setServiceWorkloadMode,
  type ServiceWorkloadMode,
  type ServiceWorkloadPendingChanges,
} from '@/lib/service-workload'
import { cn } from '@/lib/utils'

const MODES: {
  value: ServiceWorkloadMode
  title: string
  description: string
}[] = [
  {
    value: 'legacy',
    title: 'Legacy',
    description:
      'The database engine builds its own container request. This is the rollback.',
  },
  {
    value: 'shadow',
    title: 'Shadow',
    description:
      'As Legacy, and a background pass logs where the running container differs from its workload spec.',
  },
  {
    value: 'enforce',
    title: 'Enforce',
    description:
      'The container is built from its admitted workload spec the next time it is created or recreated.',
  },
]

const TITLE = 'Workload Plane'
const DESCRIPTION =
  "Choose how this database's container is built on its host, and see what a recreate from its workload spec would change."

/**
 * A database's workload mode, its pending changes and the recreate from the
 * workload spec (ADR-055 Phase 4). Instance-admin only, like the API.
 */
export function ServiceWorkloadModeCard({
  serviceId,
  serviceName,
}: {
  serviceId: number
  serviceName: string
}) {
  const isAdmin = useIsInstanceAdmin()
  const queryClient = useQueryClient()
  const [confirmMode, setConfirmMode] = useState<ServiceWorkloadMode | null>(
    null
  )
  const [recreateTarget, setRecreateTarget] =
    useState<ServiceWorkloadPendingChanges | null>(null)
  const [writeDenied, setWriteDenied] = useState<string | null>(null)

  const modeKey = serviceWorkloadModeQueryKey(serviceId)
  const pendingKey = serviceWorkloadPendingQueryKey(serviceId)
  const modeQuery = useQuery({
    queryKey: modeKey,
    queryFn: () => getServiceWorkloadMode(serviceId),
    enabled: isAdmin,
  })
  const pendingQuery = useQuery({
    queryKey: pendingKey,
    queryFn: () => getServiceWorkloadPendingChanges(serviceId),
    enabled: isAdmin && modeQuery.isSuccess,
  })

  const onWriteError = (title: string) => (error: unknown) => {
    const detail = problemDetail(error, `${title}.`)
    if (isPermissionDenied(error)) setWriteDenied(detail)
    toast.error(title, { description: detail })
  }

  const updateMode = useMutation({
    mutationFn: (mode: ServiceWorkloadMode) =>
      setServiceWorkloadMode(serviceId, mode),
    onSuccess: (change) => {
      queryClient.setQueryData(modeKey, change.workload)
      if (change.pending_changes) {
        queryClient.setQueryData(pendingKey, change.pending_changes)
      } else {
        void queryClient.invalidateQueries({ queryKey: pendingKey })
      }
      const title =
        MODES.find((mode) => mode.value === change.workload.mode)?.title ??
        change.workload.mode
      toast.success(`Workload mode set to ${title}`, {
        description: 'Nothing running has changed.',
      })
    },
    onError: onWriteError('Could not change the workload mode'),
  })

  const recreate = useMutation({
    mutationFn: (pending: ServiceWorkloadPendingChanges) =>
      recreateServiceFromWorkloadSpec(serviceId, pending.fingerprint),
    onSuccess: (result) => {
      if (result.status === 'confirmation_required') {
        queryClient.setQueryData(pendingKey, result.pending_changes)
        toast.info('Review the changes again', {
          description:
            'The server did not recreate the container and asked for a fresh confirmation of the changes now listed.',
        })
        return
      }
      const remaining = result.remaining_changes
      if (remaining) queryClient.setQueryData(pendingKey, remaining)
      else void queryClient.invalidateQueries({ queryKey: pendingKey })
      const left = remaining?.changes.length ?? 0
      if (left > 0) {
        toast.warning(`${serviceName} was recreated with differences left`, {
          description: `${left} difference${left === 1 ? '' : 's'} from the workload spec remain: ${remaining?.changes
            .map((c) => c.field)
            .join(', ')}.`,
        })
      } else {
        toast.success(`${serviceName} was recreated from its workload spec`)
      }
    },
    onError: (error) => {
      // 409: the pending changes moved since they were shown. List them
      // again so the next confirmation matches what will be applied.
      if (httpStatusOf(error) === 409) {
        void queryClient.invalidateQueries({ queryKey: pendingKey })
      }
      onWriteError('Could not recreate the container')(error)
    },
  })

  const chooseMode = (mode: ServiceWorkloadMode) => {
    if (modeQuery.data?.mode === mode) return
    if (mode === 'enforce') setConfirmMode(mode)
    else updateMode.mutate(mode)
  }

  if (!isAdmin) {
    return (
      <Section>
        <AdminOnlyNotice />
      </Section>
    )
  }

  if (modeQuery.isPending) {
    return (
      <Section>
        <div className="space-y-3" aria-busy="true">
          <Skeleton className="h-20 w-full" />
          <Skeleton className="h-24 w-full" />
        </div>
      </Section>
    )
  }

  if (modeQuery.isError) {
    return (
      <Section>
        {isPermissionDenied(modeQuery.error) ? (
          <AdminOnlyNotice
            detail={problemDetail(modeQuery.error, 'Permission denied.')}
          />
        ) : (
          <ErrorAlert
            title="Could not load the workload mode"
            description={problemDetail(
              modeQuery.error,
              'The workload mode could not be loaded.'
            )}
            retry={() => modeQuery.refetch()}
            retrying={modeQuery.isFetching}
          />
        )}
      </Section>
    )
  }

  const status = modeQuery.data
  const readOnly = writeDenied !== null
  const supported = status.support.supported
  const availability = recreateAvailability(status, pendingQuery.data)
  const changeCount = recreateTarget?.changes.length ?? 0

  return (
    <Section>
      <ConfirmDialog
        open={confirmMode !== null}
        onOpenChange={(open) => {
          if (!open) setConfirmMode(null)
        }}
        title={`Build ${serviceName} from its workload spec?`}
        description={serviceModeChangeConsequence('enforce')}
        confirmLabel="Switch to Enforce"
        destructive={false}
        onConfirm={() => {
          if (confirmMode) updateMode.mutate(confirmMode)
          setConfirmMode(null)
        }}
      />
      <ConfirmDialog
        open={recreateTarget !== null}
        onOpenChange={(open) => {
          if (!open) setRecreateTarget(null)
        }}
        title={`Recreate ${serviceName} from its workload spec?`}
        description={`The database container is stopped and replaced by one built from its workload spec, so expect brief downtime while it restarts. The replacement mounts the same data volume; the original container is kept until the replacement shows the same data, and is put back if it does not. ${changeCount} change${changeCount === 1 ? '' : 's'} listed on this page ${changeCount === 1 ? 'is' : 'are'} applied${recreateTarget ? `: ${recreateTarget.changes.map((c) => c.field).join(', ')}` : ''}.`}
        confirmLabel="Recreate container"
        onConfirm={() => {
          if (recreateTarget) recreate.mutate(recreateTarget)
          setRecreateTarget(null)
        }}
      />

      <div className="space-y-5">
        {readOnly ? (
          <Callout tone="warning" title="Read-only">
            {writeDenied}
          </Callout>
        ) : null}

        {!supported ? (
          <Callout tone="info" title="This database stays on Legacy">
            {status.support.reason ??
              'This database cannot be built from a workload spec.'}
          </Callout>
        ) : null}

        <div className="grid grid-cols-1 gap-3 md:grid-cols-3">
          {MODES.map((option) => (
            <button
              key={option.value}
              type="button"
              disabled={
                updateMode.isPending ||
                readOnly ||
                (!supported && option.value !== 'legacy')
              }
              aria-pressed={status.mode === option.value}
              onClick={() => chooseMode(option.value)}
              className={cn(
                'rounded-lg border p-4 text-left transition-colors hover:bg-muted/50 disabled:opacity-60',
                status.mode === option.value && 'border-primary bg-primary/5'
              )}
            >
              <div className="flex items-center gap-2 font-medium">
                {option.title}
                {updateMode.isPending &&
                updateMode.variables === option.value ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />
                ) : null}
              </div>
              <p className="mt-1 text-sm text-muted-foreground">
                {option.description}
              </p>
            </button>
          ))}
        </div>
        <p className="text-sm text-muted-foreground" role="status">
          {serviceWorkloadModeSummary(status)} Runs on{' '}
          {serviceLocation(status.node_id)}.
        </p>

        <PendingChangesView
          query={pendingQuery}
          onRefresh={() => void pendingQuery.refetch()}
        />

        <div className="flex flex-col gap-2 rounded-lg border p-4 sm:flex-row sm:items-center sm:justify-between">
          <div className="space-y-1">
            <p className="text-sm font-medium">
              Recreate from the workload spec
            </p>
            <p className="text-sm text-muted-foreground">
              {availability.allowed
                ? 'Applies the changes listed above. The container is recreated, with brief downtime.'
                : availability.reason}
            </p>
          </div>
          <Button
            type="button"
            variant="outline"
            className="w-full shrink-0 sm:w-auto"
            disabled={!availability.allowed || readOnly || recreate.isPending}
            onClick={() => {
              if (pendingQuery.data) setRecreateTarget(pendingQuery.data)
            }}
          >
            {recreate.isPending ? (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            ) : (
              <RotateCcw className="mr-2 h-4 w-4" />
            )}
            {recreate.isPending ? 'Recreating…' : 'Recreate container'}
          </Button>
        </div>
      </div>
    </Section>
  )
}

function Section({ children }: { children: ReactNode }) {
  return (
    <SettingsSection title={TITLE} description={DESCRIPTION} icon={Layers}>
      {children}
    </SettingsSection>
  )
}

function AdminOnlyNotice({ detail }: { detail?: string }) {
  return (
    <Callout tone="info" title="Instance administrators only">
      <p>
        An instance administrator decides how this database&apos;s container is
        built on its host: by the engine as before, or from its workload spec
        with a hardened host configuration (process limits, no-new-privileges, a
        healthcheck). Ask an administrator to change it.
      </p>
      {detail ? <p className="mt-1 text-muted-foreground">{detail}</p> : null}
    </Callout>
  )
}

function PendingChangesView({
  query,
  onRefresh,
}: {
  query: UseQueryResult<ServiceWorkloadPendingChanges, Error>
  onRefresh: () => void
}) {
  const header = (
    <div className="flex items-center justify-between gap-2">
      <div className="space-y-0.5">
        <p className="text-sm font-medium">
          What a recreate from the workload spec would change
        </p>
        {query.data ? (
          <p className="text-xs text-muted-foreground">
            Compared <TimeAgo date={query.data.computed_at} />
            {query.data.container_name
              ? ` against ${query.data.container_name}`
              : ''}
          </p>
        ) : null}
      </div>
      <Button
        type="button"
        variant="outline"
        size="sm"
        disabled={query.isFetching}
        onClick={onRefresh}
      >
        <RefreshCw
          className={cn('h-4 w-4 sm:mr-2', query.isFetching && 'animate-spin')}
        />
        <span className="max-sm:sr-only">Refresh</span>
      </Button>
    </div>
  )

  if (query.isPending) {
    return (
      <div className="space-y-2 rounded-lg border p-4" aria-busy="true">
        {header}
        <Skeleton className="h-16 w-full" />
      </div>
    )
  }
  if (query.isError) {
    return (
      <div className="space-y-2 rounded-lg border p-4">
        {header}
        <ErrorAlert
          title="Could not compare the container with its spec"
          description={problemDetail(
            query.error,
            'The pending changes could not be loaded.'
          )}
          retry={() => query.refetch()}
          retrying={query.isFetching}
        />
      </div>
    )
  }
  const pending = query.data
  return (
    <div className="space-y-3 rounded-lg border p-4">
      {header}
      {!pending.container_found ? (
        <p className="text-sm text-muted-foreground">
          {pending.support.supported
            ? `No container was found for ${pending.service_name} on ${serviceLocation(pending.node_id)}, so there is nothing to compare.`
            : (pending.support.reason ??
              'This database has no workload spec to compare against.')}
        </p>
      ) : pending.changes.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          Nothing would change: the running container already matches its
          workload spec.
        </p>
      ) : (
        <div className="rounded-md border">
          <Table className="min-w-[480px]">
            <TableHeader>
              <TableRow>
                <TableHead>Field</TableHead>
                <TableHead>Now</TableHead>
                <TableHead>After recreate</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {pending.changes.map((change) => (
                <TableRow key={change.field} className="align-top">
                  <TableCell className="font-mono text-xs">
                    {change.field}
                  </TableCell>
                  <TableCell className="whitespace-normal break-all font-mono text-xs text-muted-foreground">
                    {change.current}
                  </TableCell>
                  <TableCell className="whitespace-normal break-all font-mono text-xs">
                    {change.after_recreate}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
    </div>
  )
}
