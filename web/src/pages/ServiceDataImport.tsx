// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getDataImportAvailabilityOptions,
  getServiceOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { EmptyState } from '@/components/ui/empty-state'
import { ReadFailure } from '@/components/ui/read-failure'
import { Skeleton } from '@/components/ui/skeleton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQuery } from '@tanstack/react-query'
import { ArrowLeft, DatabaseZap, Info } from 'lucide-react'
import { useEffect } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router'
import { DataImportForm } from './service-data-import/DataImportForm'
import { DataImportRunsCard } from './service-data-import/DataImportRunsCard'
import {
  availabilityView,
  importRunPath,
} from './service-data-import/import-state'

/**
 * Import data from an external database into a database of a managed
 * service. Reached from the service's Actions menu. Rendered for every
 * service type: when the engine (or the service's state) does not allow an
 * import, the page says why instead of the feature disappearing.
 */
export function ServiceDataImport() {
  const { id } = useParams<{ id: string }>()
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const initialTarget = searchParams.get('target') ?? undefined
  const serviceId = Number(id)
  const validServiceId = Number.isSafeInteger(serviceId) && serviceId > 0
  const { setBreadcrumbs } = useBreadcrumbs()

  const serviceQuery = useQuery({
    ...getServiceOptions({ path: { id: serviceId } }),
    enabled: validServiceId,
  })
  const availabilityQuery = useQuery({
    ...getDataImportAvailabilityOptions({ path: { id: serviceId } }),
    enabled: validServiceId,
  })

  const service = serviceQuery.data?.service
  usePageTitle(service ? `Import data · ${service.name}` : 'Import data')
  useEffect(() => {
    if (!validServiceId) return
    setBreadcrumbs([
      { label: 'Databases', href: '/storage' },
      {
        label: service?.name ?? `Database ${serviceId}`,
        href: `/storage/${serviceId}`,
      },
      { label: 'Import data' },
    ])
  }, [setBreadcrumbs, service?.name, serviceId, validServiceId])

  const availability = availabilityQuery.data
  const view = availabilityView(availability)

  return (
    <PageContainer>
      <PageHeader
        title="Import data"
        description={
          service ? (
            <>
              Copy a database from another server into{' '}
              <strong>{service.name}</strong> ({service.service_type}).
            </>
          ) : (
            'Copy a database from another server into this service.'
          )
        }
        actions={
          <Button variant="outline" size="sm" asChild>
            <Link to={`/storage/${serviceId}`}>
              <ArrowLeft className="h-4 w-4 sm:mr-2" />
              <span className="hidden sm:inline">Back to service</span>
            </Link>
          </Button>
        }
      />

      {availabilityQuery.isError ? (
        <ReadFailure
          resource="import availability"
          error={availabilityQuery.error}
          onRetry={() => void availabilityQuery.refetch()}
          retrying={availabilityQuery.isFetching}
        />
      ) : view.kind === 'loading' ? (
        <Card>
          <CardHeader>
            <Skeleton className="h-6 w-64" />
            <Skeleton className="h-4 w-full max-w-xl" />
          </CardHeader>
          <CardContent className="space-y-4">
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-16 w-full" />
          </CardContent>
        </Card>
      ) : view.kind === 'unsupported' ? (
        <EmptyState
          icon={DatabaseZap}
          title="This service cannot receive imported data"
          description={
            <>
              {capitalize(view.reason)}. Imports work for standalone PostgreSQL,
              MariaDB, MongoDB and Redis services on this server — create one
              under{' '}
              <Link to="/storage" className="underline">
                Databases
              </Link>{' '}
              and import into it.
            </>
          }
        />
      ) : (
        <div className="space-y-6">
          {view.kind === 'unavailable' && (
            <Alert variant="warning">
              <Info className="h-4 w-4" />
              <AlertTitle>Imports cannot start right now</AlertTitle>
              <AlertDescription>
                {capitalize(view.reason)}.{' '}
                <Link to={`/storage/${serviceId}`} className="underline">
                  Open the service
                </Link>
              </AlertDescription>
            </Alert>
          )}
          {availability?.spec && (
            <DataImportForm
              serviceId={serviceId}
              serviceType={availability.service_type}
              spec={availability.spec}
              available={view.kind === 'ready'}
              defaultTimeoutMinutes={availability.default_timeout_minutes}
              maxTimeoutMinutes={availability.max_timeout_minutes}
              onStarted={(run) => navigate(importRunPath(serviceId, run.id))}
              initialTarget={initialTarget}
            />
          )}
          <DataImportRunsCard
            serviceId={serviceId}
            objectNoun={availability?.spec?.object_noun ?? 'table'}
          />
        </div>
      )}
    </PageContainer>
  )
}

function capitalize(text: string): string {
  const trimmed = text.replace(/\.$/, '')
  return trimmed.charAt(0).toUpperCase() + trimmed.slice(1)
}
