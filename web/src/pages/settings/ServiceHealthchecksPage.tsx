// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useMemo } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { PageHeader, SettingsGroup } from '@temps-sdk/ds'
import { toast } from 'sonner'
import {
  getHealthcheckDefaultsOptions,
  getHealthcheckDefaultsQueryKey,
  updateHealthcheckDefaultsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { HealthcheckDefaults } from '@/api/client/types.gen'
import { SettingsFormSkeleton } from '@/components/settings/SettingsFormSkeleton'
import { SettingsLoadFailure } from '@/components/settings/SettingsLoadFailure'
import { HealthcheckOverridesForm } from '@/components/storage/HealthcheckOverridesForm'
import { Badge } from '@/components/ui/badge'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useIsInstanceAdmin } from '@/hooks/useIsInstanceAdmin'
import { usePageTitle } from '@/hooks/usePageTitle'
import { SETTINGS_REFUSAL } from '@/hooks/useSettings'
import { problemDetail } from '@/lib/api-problem'
import { isForbiddenRead } from '@/lib/read-failure'
import {
  APPLY_ON_RECREATE,
  checksPerMinute,
  engineLabel,
  formatHealthcheckValue,
  HEALTHCHECK_FIELDS,
  sourceLabel,
} from '@/lib/service-healthcheck'

const TITLE = 'Service health checks'

/** What each engine's containers get with the instance default. */
export function EngineHealthcheckTable({
  defaults,
}: {
  defaults: HealthcheckDefaults
}) {
  return (
    <div className="@container">
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Engine</TableHead>
            {HEALTHCHECK_FIELDS.map((spec) => (
              <TableHead
                key={spec.field}
                className={
                  spec.field === 'interval_secs'
                    ? undefined
                    : 'hidden @xl:table-cell'
                }
              >
                {spec.label}
              </TableHead>
            ))}
          </TableRow>
        </TableHeader>
        <TableBody>
          {defaults.engines.map((row) => (
            <TableRow key={row.engine}>
              <TableCell className="font-medium">
                {engineLabel(row.engine)}
                <span className="block text-xs font-normal text-muted-foreground @xl:hidden">
                  Timeout{' '}
                  {formatHealthcheckValue(
                    'timeout_secs',
                    row.effective.timings.timeout_secs
                  )}
                  , start period{' '}
                  {formatHealthcheckValue(
                    'start_period_secs',
                    row.effective.timings.start_period_secs
                  )}
                </span>
              </TableCell>
              {HEALTHCHECK_FIELDS.map((spec) => (
                <TableCell
                  key={spec.field}
                  className={
                    spec.field === 'interval_secs'
                      ? undefined
                      : 'hidden @xl:table-cell'
                  }
                >
                  <span className="block">
                    {formatHealthcheckValue(
                      spec.field,
                      row.effective.timings[spec.field]
                    )}
                    {spec.field === 'interval_secs' ? (
                      <span className="text-xs text-muted-foreground">
                        {' '}
                        ({checksPerMinute(row.effective.timings.interval_secs)}
                        /min)
                      </span>
                    ) : null}
                  </span>
                  {row.effective.sources[spec.field] !== 'builtin' ? (
                    <Badge variant="outline" className="mt-1 font-normal">
                      {sourceLabel(row.effective.sources[spec.field])}
                    </Badge>
                  ) : null}
                </TableCell>
              ))}
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  )
}

/** The instance-wide default healthcheck of managed service containers. */
export function ServiceHealthchecksPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const isAdmin = useIsInstanceAdmin()
  const queryClient = useQueryClient()
  const queryKey = getHealthcheckDefaultsQueryKey()
  // Reading the default needs settings:read, which only administrators
  // hold; any other role would be refused every time.
  const query = useQuery({
    ...getHealthcheckDefaultsOptions(),
    enabled: isAdmin,
  })
  const error = isAdmin ? query.error : SETTINGS_REFUSAL

  useEffect(() => {
    setBreadcrumbs([{ label: 'Settings', href: '/settings' }, { label: TITLE }])
  }, [setBreadcrumbs])
  usePageTitle(TITLE)

  const save = useMutation({
    ...updateHealthcheckDefaultsMutation(),
    onSuccess: (result) => {
      queryClient.setQueryData(queryKey, result)
      toast.success('Health check defaults saved', {
        description: APPLY_ON_RECREATE,
      })
    },
    onError: (failure) =>
      toast.error('Could not save the health check defaults', {
        description: problemDetail(failure, 'The defaults were not changed.'),
      }),
    onSettled: () => void queryClient.invalidateQueries({ queryKey }),
  })

  const builtins = useMemo(
    () => query.data?.engines.map((row) => row.builtin) ?? [],
    [query.data]
  )

  if (error) {
    return (
      <SettingsLoadFailure
        error={error}
        onRetry={
          isForbiddenRead(error) ? undefined : () => void query.refetch()
        }
      />
    )
  }

  const defaults = query.data
  if (!defaults) {
    return (
      <SettingsFormSkeleton
        title={TITLE}
        label="Loading health check defaults"
      />
    )
  }

  return (
    <div className="space-y-10">
      <PageHeader title={TITLE} />
      <SettingsGroup
        title="Instance default"
        description="Every managed database and storage service uses these unless it sets its own."
      >
        <HealthcheckOverridesForm
          overrides={defaults.overrides}
          inherited={builtins}
          inheritedFrom="each engine's built-in value"
          saving={save.isPending}
          onSave={(overrides) => save.mutate({ body: overrides })}
        />
      </SettingsGroup>
      <section aria-labelledby="healthcheck-engines" className="space-y-3">
        <h2 id="healthcheck-engines" className="text-lg font-semibold">
          What each engine gets
        </h2>
        <EngineHealthcheckTable defaults={defaults} />
      </section>
    </div>
  )
}
