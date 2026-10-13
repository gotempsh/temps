// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo } from 'react'
import { Link } from 'react-router'
import { useQueries, useQuery } from '@tanstack/react-query'
import { subDays } from 'date-fns'
import { Database, ExternalLink, Plus, RefreshCw } from 'lucide-react'
import {
  getProjectsQueryKey,
  listProjectServicesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { getProjects } from '@/api/client/sdk.gen'
import type {
  ProjectGroupResponse,
  ProjectResponse,
  ServiceTypeRoute,
} from '@/api/client'
import { ProjectCard } from '@/components/dashboard/ProjectCard'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { ServiceLogo } from '@/components/ui/service-logo'
import { useDashboardAnalytics } from '@/hooks/useDashboardAnalytics'
import { useDashboardHealth } from '@/hooks/useDashboardHealth'
import { useProjectsMonitorHealth } from '@/hooks/useProjectsMonitorHealth'
import { useLatestDeploymentMedia } from '@/hooks/useLatestDeploymentMedia'
import { getErrorMessage } from '@/utils/errorHandling'
import { GroupRoutesSummary } from './GroupRoutesSummary'
import { UnfinishedImportsNotice } from './UnfinishedImportsNotice'
import {
  environmentLink,
  primaryEnvironment,
  useServiceEnvironments,
  type ServiceLink,
} from './service-urls'

/** Members per request; the API caps a page at 100. */
const MEMBER_PAGE_SIZE = 100
const MAX_MEMBER_PAGES = 20

/** Every member of a group as full services, across all pages. */
async function fetchGroupMembers(groupId: number, signal: AbortSignal) {
  const members: ProjectResponse[] = []
  for (let page = 1; page <= MAX_MEMBER_PAGES; page++) {
    const { data } = await getProjects({
      query: { project_group_id: groupId, page, per_page: MEMBER_PAGE_SIZE },
      signal,
      throwOnError: true,
    })
    members.push(...data.projects)
    if (
      data.projects.length < MEMBER_PAGE_SIZE ||
      members.length >= data.total
    ) {
      break
    }
  }
  return members
}

interface GroupServicesPanelProps {
  group: ProjectGroupResponse
  onAddServices: () => void
}

export function GroupServicesPanel({
  group,
  onAddServices,
}: GroupServicesPanelProps) {
  const memberIds = useMemo(
    () => group.projects.map((p) => p.id),
    [group.projects]
  )

  // ProjectCard needs the full service; the group only carries a summary.
  const catalog = useQuery({
    queryKey: getProjectsQueryKey({
      query: { project_group_id: group.id, per_page: MEMBER_PAGE_SIZE },
    }),
    queryFn: ({ signal }) => fetchGroupMembers(group.id, signal),
    enabled: memberIds.length > 0,
  })
  const members: ProjectResponse[] = useMemo(() => {
    const byId = new Map((catalog.data ?? []).map((p) => [p.id, p]))
    return memberIds
      .map((id) => byId.get(id))
      .filter((p): p is ProjectResponse => Boolean(p))
  }, [catalog.data, memberIds])

  const { startDate, endDate } = useMemo(
    () => ({
      startDate: subDays(new Date(), 1).toISOString(),
      endDate: new Date().toISOString(),
    }),
    []
  )
  const analytics = useDashboardAnalytics(memberIds, startDate, endDate)
  const health = useDashboardHealth(memberIds, startDate, endDate)
  const monitorHealth = useProjectsMonitorHealth(memberIds)
  const media = useLatestDeploymentMedia(memberIds)
  const environments = useServiceEnvironments(memberIds)
  const links = new Map(
    memberIds.map((id) => [
      id,
      environmentLink(primaryEnvironment(environments.byProject.get(id))),
    ])
  )

  return (
    <div className="space-y-8">
      <UnfinishedImportsNotice group={group} />
      <section aria-labelledby="group-services" className="space-y-3">
        <div className="flex items-end justify-between border-b pb-2">
          <div>
            <h2 id="group-services" className="font-semibold">
              Services
            </h2>
            <p className="text-sm text-muted-foreground">
              Each service keeps its own deployments, domains and settings.
            </p>
          </div>
        </div>

        {group.projects.length === 0 ? (
          <div className="rounded-xl border border-dashed px-6 py-10 text-center">
            <p className="font-medium">No services in this project yet</p>
            <p className="mt-1 text-sm text-muted-foreground">
              Add the services that ship together, for example a frontend, an
              API and a worker.
            </p>
            <Button
              className="mt-4"
              variant="outline"
              size="sm"
              onClick={onAddServices}
            >
              <Plus className="size-4" />
              Add services
            </Button>
          </div>
        ) : catalog.isLoading ? (
          <div className="space-y-2">
            {memberIds.map((id) => (
              <Skeleton key={id} className="h-14 w-full" />
            ))}
          </div>
        ) : catalog.isError && !catalog.data ? (
          <Alert variant="destructive">
            <AlertTitle>Services could not be loaded</AlertTitle>
            <AlertDescription>
              <p>
                {getErrorMessage(
                  catalog.error,
                  'Check your connection and retry.'
                )}
              </p>
              <Button
                variant="outline"
                size="sm"
                className="mt-3"
                disabled={catalog.isFetching}
                onClick={() => void catalog.refetch()}
              >
                <RefreshCw
                  className={
                    catalog.isFetching ? 'size-4 animate-spin' : 'size-4'
                  }
                />
                Retry
              </Button>
            </AlertDescription>
          </Alert>
        ) : (
          <div className="divide-y rounded-lg border">
            {members.map((project) => (
              <div key={project.id} className="flex items-stretch">
                <div className="min-w-0 flex-1">
                  <ProjectCard
                    project={project}
                    layout="dense"
                    analytics={analytics.data?.projects?.[String(project.id)]}
                    analyticsLoading={analytics.isLoading}
                    analyticsError={analytics.isError}
                    health={health.data?.projects?.[String(project.id)]}
                    healthLoading={health.isLoading}
                    healthError={health.isError}
                    monitorHealth={
                      monitorHealth.data?.projects?.[String(project.id)]
                    }
                    latestDeploymentMedia={
                      media.data?.projects?.[String(project.id)]
                    }
                    latestDeploymentMediaLoading={media.isLoading}
                    latestDeploymentMediaError={media.isError && !media.data}
                    domain={links.get(project.id)?.label}
                  />
                </div>
                <VisitServiceLink
                  name={project.name}
                  link={links.get(project.id)}
                />
              </div>
            ))}
            {members.length < memberIds.length && (
              <p className="px-3 py-2 text-xs text-muted-foreground">
                {memberIds.length - members.length === 1
                  ? '1 more service is'
                  : `${memberIds.length - members.length} more services are`}{' '}
                in this project but could not be loaded in full:{' '}
                {group.projects
                  .filter((p) => !members.some((m) => m.id === p.id))
                  .map((p, i) => (
                    <span key={p.id}>
                      {i > 0 && ', '}
                      <Link className="underline" to={`/projects/${p.slug}`}>
                        {p.name}
                      </Link>
                    </span>
                  ))}
              </p>
            )}
          </div>
        )}
      </section>

      <GroupRoutesSummary group={group} />

      <GroupDatabases group={group} />
    </div>
  )
}

/** Opens the running service, like the button in its header. */
function VisitServiceLink({
  name,
  link,
}: {
  name: string
  link?: ServiceLink | null
}) {
  if (!link) {
    return <span className="w-10 shrink-0" aria-hidden="true" />
  }
  return (
    <a
      href={link.href}
      target="_blank"
      rel="noopener noreferrer"
      title={`Open ${link.label}`}
      aria-label={`Open ${name} at ${link.label} in a new window`}
      className="flex w-10 shrink-0 items-center justify-center text-muted-foreground transition-colors hover:bg-muted/50 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
    >
      <ExternalLink className="size-4" />
    </a>
  )
}

/** Databases linked to any service in the group, with who uses each. */
function GroupDatabases({ group }: { group: ProjectGroupResponse }) {
  const results = useQueries({
    queries: group.projects.map((p) => ({
      ...listProjectServicesOptions({ path: { project_id: p.id } }),
      retry: false,
    })),
  })
  const loading = results.some((r) => r.isLoading)
  const failed = results.filter((r) => r.isError).length

  const databases = useMemo(() => {
    const byId = new Map<
      number,
      { id: number; name: string; type: ServiceTypeRoute; usedBy: string[] }
    >()
    results.forEach((result, index) => {
      const member = group.projects[index]
      for (const link of result.data ?? []) {
        const entry = byId.get(link.service.id) ?? {
          id: link.service.id,
          name: link.service.name,
          type: link.service.service_type,
          usedBy: [],
        }
        if (member) entry.usedBy.push(member.name)
        byId.set(link.service.id, entry)
      }
    })
    return [...byId.values()].sort((a, b) => a.name.localeCompare(b.name))
  }, [group.projects, results])

  return (
    <section aria-labelledby="group-databases" className="space-y-3">
      <div className="border-b pb-2">
        <h2 id="group-databases" className="font-semibold">
          Databases
        </h2>
        <p className="text-sm text-muted-foreground">
          Linked to at least one service in this project. Link or create
          databases from a service&apos;s Databases tab.
        </p>
      </div>
      {loading ? (
        <Skeleton className="h-14 w-full" />
      ) : databases.length === 0 ? (
        <p className="rounded-lg border border-dashed px-4 py-6 text-center text-sm text-muted-foreground">
          {failed > 0
            ? `Linked databases could not be loaded for ${failed} ${failed === 1 ? 'service' : 'services'}.`
            : 'No databases are linked to these services yet.'}
        </p>
      ) : (
        <div className="divide-y rounded-lg border">
          {databases.map((db) => (
            <Link
              key={db.id}
              to={`/storage/${db.id}`}
              className="flex items-center gap-3 px-3 py-2.5 transition-colors hover:bg-muted/50"
            >
              <div className="flex size-8 shrink-0 items-center justify-center rounded-md bg-muted">
                <ServiceLogo service={db.type} className="size-4" />
              </div>
              <div className="min-w-0 flex-1">
                <p className="truncate text-sm font-medium">{db.name}</p>
                <p className="truncate text-xs text-muted-foreground">
                  Used by {db.usedBy.join(', ')}
                </p>
              </div>
              <Database className="size-4 text-muted-foreground" />
            </Link>
          ))}
          {failed > 0 && (
            <p className="px-3 py-2 text-xs text-destructive">
              Links for {failed} {failed === 1 ? 'service' : 'services'} could
              not be loaded, so this list may be incomplete.
            </p>
          )}
        </div>
      )}
    </section>
  )
}
