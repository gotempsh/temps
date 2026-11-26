// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { Link } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { ChevronRight } from 'lucide-react'
import { getProvidersMetadataOptions } from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { ServiceLogo } from '@/components/ui/service-logo'
import { serviceCreateHref } from '@/lib/service-project-link'

/**
 * One card per database type the server can run. Each opens the create
 * form, which links the new database to `projectId` once it exists.
 */
export function DatabaseTypeCards({
  projectId,
  heading,
  description,
  controls,
}: {
  projectId: number | null
  heading: string
  description: ReactNode
  /** Shown between the heading and the cards, e.g. which service to link. */
  controls?: ReactNode
}) {
  const providers = useQuery({ ...getProvidersMetadataOptions(), retry: false })

  return (
    <section aria-label={heading} className="space-y-4">
      <div>
        <h2 className="text-lg font-semibold">{heading}</h2>
        <p className="mt-1 text-sm text-muted-foreground">{description}</p>
      </div>
      {controls}
      {providers.isPending ? (
        <div
          role="status"
          aria-label="Loading database types"
          className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3"
        >
          {Array.from({ length: 5 }, (_, index) => (
            <div
              key={index}
              className="h-40 animate-pulse rounded-lg bg-muted"
            />
          ))}
        </div>
      ) : providers.isError ? (
        <Alert variant="destructive">
          <AlertTitle>Database types could not be loaded</AlertTitle>
          <AlertDescription>
            <Button
              variant="outline"
              size="sm"
              onClick={() => void providers.refetch()}
            >
              Retry
            </Button>
          </AlertDescription>
        </Alert>
      ) : providers.data?.length ? (
        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
          {providers.data.map((provider) => (
            <Link
              key={provider.service_type}
              to={serviceCreateHref(provider.service_type, projectId)}
              className="group flex min-w-0 flex-col items-start rounded-lg border bg-background p-5 transition-colors hover:border-foreground/30 hover:bg-muted/40 focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring"
            >
              <div className="mb-4 flex size-10 items-center justify-center">
                <ServiceLogo service={provider.service_type} />
              </div>
              <h3 className="font-medium">{provider.display_name}</h3>
              <p className="mt-1 flex-1 text-sm text-muted-foreground">
                {provider.description}
              </p>
              <span className="mt-5 inline-flex items-center gap-2 text-sm font-medium">
                Create database{' '}
                <ChevronRight
                  aria-hidden="true"
                  className="size-4 transition-transform group-hover:translate-x-0.5"
                />
              </span>
            </Link>
          ))}
        </div>
      ) : (
        <p className="text-sm text-muted-foreground">
          No database types are available.
        </p>
      )}
    </section>
  )
}
