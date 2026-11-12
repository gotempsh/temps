// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Callout, Status } from '@temps-sdk/ds'
import { HeartPulse, Loader2, RotateCw } from 'lucide-react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import {
  applyServiceHealthcheckMutation,
  getServiceHealthcheckOptions,
  getServiceHealthcheckQueryKey,
  updateServiceHealthcheckMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  HealthcheckOverrides,
  ServiceHealthcheck,
} from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { ConfirmDialog } from '@/components/ui/confirm-dialog'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { useIsInstanceAdmin } from '@/hooks/useIsInstanceAdmin'
import { problemDetail } from '@/lib/api-problem'
import {
  APPLY_ON_RECREATE,
  applyNowCopy,
  checksPerMinute,
  formatHealthcheckValue,
  HEALTHCHECK_FIELDS,
  sourceLabel,
} from '@/lib/service-healthcheck'
import { HealthcheckOverridesForm } from './HealthcheckOverridesForm'

export const HEALTHCHECK_DEFAULTS_HREF = '/settings/service-healthchecks'

export interface ServiceHealthcheckViewProps {
  healthcheck: ServiceHealthcheck
  saving?: boolean
  applying?: boolean
  onSave: (overrides: HealthcheckOverrides) => void
  /** Called once the operator confirmed the restart. */
  onApply: () => void
  /** Why the last save failed, shown beside Save. */
  saveError?: string | null
  /** Told when a field changes, to clear `saveError`. */
  onEdit?: () => void
  /** Whether the viewer can open the instance default (settings:read). */
  canReadDefaults?: boolean
}

/** A service's effective healthcheck, its overrides and Apply now. */
export function ServiceHealthcheckView({
  healthcheck,
  saving = false,
  applying = false,
  onSave,
  onApply,
  saveError,
  onEdit,
  canReadDefaults = false,
}: ServiceHealthcheckViewProps) {
  const [confirming, setConfirming] = useState(false)
  const [dirty, setDirty] = useState(false)
  const inherited = useMemo(
    () =>
      healthcheck.inherited
        ? [
            {
              timings: healthcheck.inherited.timings,
              readinessWaitSecs:
                healthcheck.readiness_wait_secs ??
                healthcheck.inherited.timings.start_period_secs,
            },
          ]
        : [],
    [healthcheck.inherited, healthcheck.readiness_wait_secs]
  )
  const onDirtyChange = useCallback((d: boolean) => setDirty(d), [])
  const { capability, effective, running } = healthcheck

  if (!capability.configurable || !effective) {
    return (
      <Callout tone="info" title="Health check settings are not available">
        {capability.reason
          ? `${healthcheck.service_name} has no configurable health check: ${capability.reason}.`
          : `${healthcheck.service_name} has no configurable health check.`}
      </Callout>
    )
  }

  const copy = applyNowCopy(healthcheck.service_name)
  const busy = saving || applying
  const applyBlocked = !capability.can_apply_now
    ? (capability.apply_now_reason ?? 'Temps cannot recreate it from here')
    : dirty
      ? 'Save your changes first'
      : null

  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-center gap-2" role="status">
        {healthcheck.pending_recreate ? (
          <Status tone="warn" label="Pending recreate" />
        ) : running ? (
          <Status tone="ok" label="Applied" />
        ) : null}
        <span className="text-sm text-muted-foreground">
          {running
            ? `The running container is checked every ${formatHealthcheckValue('interval_secs', running.interval_secs)} (${checksPerMinute(running.interval_secs)} per minute).`
            : APPLY_ON_RECREATE}
        </span>
      </div>

      <dl className="grid grid-cols-2 gap-3 text-sm md:grid-cols-5">
        {HEALTHCHECK_FIELDS.map((spec) => {
          const value = effective.timings[spec.field]
          const runningValue = running?.[spec.field]
          return (
            <div key={spec.field} className="space-y-1">
              <dt className="text-muted-foreground">{spec.label}</dt>
              <dd className="font-medium">
                {formatHealthcheckValue(spec.field, value)}
              </dd>
              <dd>
                <Badge variant="outline" className="font-normal">
                  {sourceLabel(effective.sources[spec.field])}
                </Badge>
              </dd>
              {runningValue !== undefined && runningValue !== value ? (
                <dd className="text-xs text-muted-foreground">
                  Running: {formatHealthcheckValue(spec.field, runningValue)}
                </dd>
              ) : null}
            </div>
          )
        })}
      </dl>

      {healthcheck.overrides_ignored_reason ? (
        <Callout
          tone="warning"
          title="This service's overrides are not being used"
        >
          They no longer combine with the instance default, so new containers
          get the values above without them:{' '}
          {healthcheck.overrides_ignored_reason}. Change or clear them below.
        </Callout>
      ) : null}

      {healthcheck.start_interval_supported === false ? (
        <Callout
          tone="warning"
          title="This host's Docker ignores the start interval"
        >
          Containers here are checked every{' '}
          {formatHealthcheckValue(
            'start_interval_secs',
            effective.timings.start_interval_secs
          )}{' '}
          for as long as they run. Docker Engine 25 or later checks them every{' '}
          {formatHealthcheckValue(
            'interval_secs',
            effective.timings.interval_secs
          )}{' '}
          once healthy.
        </Callout>
      ) : null}

      <HealthcheckOverridesForm
        // Another service's page reuses this card: its edits must not carry
        // over into this service's form.
        key={healthcheck.service_id}
        overrides={healthcheck.overrides}
        inherited={inherited}
        inheritedFrom="the instance default"
        saving={saving}
        disabled={applying}
        submitLabel="Save overrides"
        onSave={onSave}
        onDirtyChange={onDirtyChange}
        saveError={saveError}
        onEdit={onEdit}
      />

      <div className="flex flex-col gap-3 rounded-lg border p-4 sm:flex-row sm:items-center sm:justify-between">
        <div className="min-w-0 space-y-1">
          <p className="text-sm font-medium">{APPLY_ON_RECREATE}</p>
          <p className="text-xs text-muted-foreground">
            {applyBlocked ??
              'Apply now recreates the container; it restarts briefly.'}{' '}
            {canReadDefaults ? (
              <Link
                to={HEALTHCHECK_DEFAULTS_HREF}
                className="text-primary hover:text-primary/80"
              >
                Instance default
              </Link>
            ) : (
              'An administrator sets the instance default.'
            )}
          </p>
        </div>
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="shrink-0"
          disabled={busy || applyBlocked !== null}
          onClick={() => setConfirming(true)}
        >
          {applying ? (
            <Loader2 className="mr-2 h-4 w-4 animate-spin" />
          ) : (
            <RotateCw className="mr-2 h-4 w-4" />
          )}
          Apply now
        </Button>
      </div>

      <ConfirmDialog
        open={confirming}
        onOpenChange={setConfirming}
        title={copy.title}
        description={copy.description}
        confirmLabel={copy.confirmLabel}
        destructive={false}
        onConfirm={() => {
          setConfirming(false)
          onApply()
        }}
      />
    </div>
  )
}

const TITLE = 'Health checks'

/** The Docker healthcheck of a managed service's container. */
export function ServiceHealthcheckCard({
  serviceId,
  serviceName,
  serviceStatus,
}: {
  serviceId: number
  serviceName: string
  /** The service's lifecycle status; a change re-reads whether Apply now
   * is possible (it is not while the service is stopped). */
  serviceStatus?: string
}) {
  const queryClient = useQueryClient()
  const canReadDefaults = useIsInstanceAdmin()
  const seenStatus = useRef(serviceStatus)
  useEffect(() => {
    if (seenStatus.current === serviceStatus) return
    seenStatus.current = serviceStatus
    void queryClient.invalidateQueries({
      queryKey: getServiceHealthcheckQueryKey({ path: { id: serviceId } }),
    })
  }, [serviceStatus, serviceId, queryClient])
  const queryKey = getServiceHealthcheckQueryKey({ path: { id: serviceId } })
  const query = useQuery(
    getServiceHealthcheckOptions({ path: { id: serviceId } })
  )

  const save = useMutation({
    ...updateServiceHealthcheckMutation(),
    onSuccess: (result) => {
      queryClient.setQueryData(queryKey, result)
      toast.success(`Health check settings saved for ${serviceName}`, {
        description: APPLY_ON_RECREATE,
      })
    },
    onError: (error) =>
      toast.error('Could not save the health check settings', {
        description: problemDetail(error, 'The settings were not changed.'),
      }),
    onSettled: () => void queryClient.invalidateQueries({ queryKey }),
  })

  const apply = useMutation({
    ...applyServiceHealthcheckMutation(),
    onSuccess: (result) => {
      // Null when it could not be read back; onSettled reads it again.
      if (result.healthcheck)
        queryClient.setQueryData(queryKey, result.healthcheck)
      toast.success(`${serviceName} was recreated with its health check`)
    },
    onError: (error) =>
      toast.error(`Could not recreate ${serviceName}`, {
        description: problemDetail(error, 'The original container was kept.'),
      }),
    onSettled: () => void queryClient.invalidateQueries({ queryKey }),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <HeartPulse className="h-4 w-4" />
          {TITLE}
        </CardTitle>
      </CardHeader>
      <CardContent>
        {query.isPending ? (
          <div className="space-y-4" aria-busy="true">
            <Skeleton className="h-5 w-72" />
            <div className="grid grid-cols-2 gap-3 md:grid-cols-5">
              {HEALTHCHECK_FIELDS.map((spec) => (
                <Skeleton key={spec.field} className="h-14 w-full" />
              ))}
            </div>
            <Skeleton className="h-9 w-full" />
          </div>
        ) : query.data ? (
          <div className="space-y-4">
            {/* A failed background refresh keeps the form and its edits. */}
            {query.isError ? (
              <Callout
                tone="error"
                title="Could not refresh the health check settings"
              >
                {problemDetail(
                  query.error,
                  'Showing the settings as last loaded.'
                )}
              </Callout>
            ) : null}
            <ServiceHealthcheckView
              healthcheck={query.data}
              saving={save.isPending}
              applying={apply.isPending}
              saveError={
                save.isError
                  ? problemDetail(save.error, 'The settings were not changed.')
                  : null
              }
              onEdit={save.reset}
              canReadDefaults={canReadDefaults}
              onSave={(overrides) =>
                save.mutate({ path: { id: serviceId }, body: overrides })
              }
              onApply={() => apply.mutate({ path: { id: serviceId } })}
            />
          </div>
        ) : (
          <ErrorAlert
            title="Could not load the health check settings"
            description={problemDetail(
              query.error,
              'The health check settings could not be loaded.'
            )}
            retry={() => query.refetch()}
            retrying={query.isFetching}
          />
        )}
      </CardContent>
    </Card>
  )
}
