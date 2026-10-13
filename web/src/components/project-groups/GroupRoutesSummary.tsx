// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Link } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import {
  AlertTriangle,
  ArrowRight,
  ExternalLink,
  Waypoints,
} from 'lucide-react'
import { listGroupRoutesOptions } from '@/api/client/@tanstack/react-query.gen'
import type {
  EnvironmentResponse,
  GroupRouteResponse,
  ProjectGroupResponse,
} from '@/api/client'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { getErrorMessage } from '@/utils/errorHandling'
import { environmentLink, useServiceEnvironments } from './service-urls'

const SETTINGS = { search: '?tab=settings&section=routes' }

/** How requests move between the project's services, at a glance. */
export function GroupRoutesSummary({ group }: { group: ProjectGroupResponse }) {
  const routes = useQuery(
    listGroupRoutesOptions({ path: { group_id: group.id } })
  )
  const environments = useServiceEnvironments(group.projects.map((p) => p.id))
  const slugOf = (id: number) => group.projects.find((p) => p.id === id)?.slug
  // With a single service there is nothing to route between.
  if (group.projects.length < 2 && !routes.data?.length) return null

  return (
    <section aria-labelledby="group-routes" className="space-y-3">
      <div className="flex items-end justify-between gap-4 border-b pb-2">
        <div>
          <h2 id="group-routes" className="font-semibold">
            Path routes
          </h2>
          <p className="text-sm text-muted-foreground">
            Paths of one service's domains served by another service.
          </p>
        </div>
        <Button variant="ghost" size="sm" asChild>
          <Link to={SETTINGS}>Manage</Link>
        </Button>
      </div>
      {routes.isLoading ? (
        <Skeleton className="h-12 w-full" />
      ) : routes.isError ? (
        <div
          role="alert"
          className="flex items-center gap-2 rounded-lg border px-3 py-2.5 text-sm text-destructive"
        >
          <AlertTriangle className="size-4 shrink-0" />
          <span className="flex-1">
            {getErrorMessage(routes.error, 'Routes could not be loaded.')}
          </span>
          <Button variant="outline" size="sm" onClick={() => routes.refetch()}>
            Retry
          </Button>
        </div>
      ) : (routes.data ?? []).length === 0 ? (
        <p className="rounded-lg border border-dashed px-4 py-4 text-sm text-muted-foreground">
          Every service answers only on its own domains. Does a frontend call a
          same-origin <code>/api</code>?{' '}
          <Link
            to={SETTINGS}
            className="font-medium text-foreground hover:underline"
          >
            Route it to the API service
          </Link>{' '}
          instead of setting up CORS or a second domain.
        </p>
      ) : (
        <div className="divide-y rounded-lg border">
          {(routes.data ?? []).map((route) => (
            <RouteSummaryRow
              key={route.id}
              route={route}
              slugOf={slugOf}
              environmentsById={environments.byId}
            />
          ))}
        </div>
      )}
    </section>
  )
}

function RouteSummaryRow({
  route,
  slugOf,
  environmentsById,
}: {
  route: GroupRouteResponse
  slugOf: (id: number) => string | undefined
  environmentsById: Map<number, EnvironmentResponse>
}) {
  const service = (id: number, name: string) => {
    const slug = slugOf(id)
    return slug ? (
      <Link to={`/projects/${slug}`} className="font-medium hover:underline">
        {name}
      </Link>
    ) : (
      <span className="font-medium">{name}</span>
    )
  }
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2.5 text-sm">
      <Waypoints
        className="size-4 shrink-0 text-muted-foreground"
        aria-hidden="true"
      />
      {service(route.source_project_id, route.source_project_name)}
      <code className="rounded bg-muted px-1.5 py-0.5 text-xs">
        {route.path_prefix}
      </code>
      <ArrowRight
        className="size-3.5 text-muted-foreground"
        aria-hidden="true"
      />
      {service(route.target_project_id, route.target_project_name)}
      {route.strip_prefix && (
        <span className="text-xs text-muted-foreground">prefix removed</span>
      )}
      <RouteAddresses
        route={route}
        environmentsById={environmentsById}
        className="w-full pl-7"
      />
    </div>
  )
}

/**
 * Where each environment's rule answers, as real links: the source
 * environment's domain plus the prefix. Flags environments whose requests
 * get a 502 because the target has nothing deployed there.
 */
export function RouteAddresses({
  route,
  environmentsById,
  className,
}: {
  route: GroupRouteResponse
  environmentsById: Map<number, EnvironmentResponse>
  className?: string
}) {
  if (route.environments.length === 0) {
    return (
      <p className={`text-xs text-muted-foreground ${className ?? ''}`}>
        Applies once {route.source_project_name} has an environment.
      </p>
    )
  }
  const labelled = route.environments.length > 1
  return (
    <ul className={`flex flex-wrap gap-x-4 gap-y-1 text-xs ${className ?? ''}`}>
      {route.environments.map((e) => {
        const link = environmentLink(
          environmentsById.get(e.source_environment_id),
          route.path_prefix
        )
        const retargeted =
          e.target_environment_slug &&
          e.target_environment_slug !== e.source_environment_slug
        return (
          <li
            key={e.source_environment_id}
            className="flex min-w-0 items-center gap-1.5"
          >
            {labelled && (
              <span className="text-muted-foreground">
                {e.source_environment_slug}:
              </span>
            )}
            {link ? (
              <a
                href={link.href}
                target="_blank"
                rel="noopener noreferrer"
                className="flex min-w-0 items-center gap-1 font-mono text-muted-foreground hover:text-foreground hover:underline"
              >
                <span className="truncate">{link.label}</span>
                <ExternalLink className="size-3 shrink-0" aria-hidden="true" />
              </a>
            ) : (
              <span className="text-muted-foreground">
                {e.source_environment_slug} (no address yet)
              </span>
            )}
            {!e.target_environment_slug ? (
              <span className="text-amber-700 dark:text-amber-500">
                → no matching {route.target_project_name} environment
              </span>
            ) : !e.target_deployed ? (
              <span className="flex items-center gap-1 text-amber-700 dark:text-amber-500">
                <AlertTriangle className="size-3" aria-hidden="true" />
                502 until {route.target_project_name} is deployed in{' '}
                {e.target_environment_slug}
              </span>
            ) : (
              retargeted && (
                <span className="text-muted-foreground">
                  → {e.target_environment_slug}
                </span>
              )
            )}
          </li>
        )
      })}
    </ul>
  )
}
