// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * A project's own lists (deployments, domains, databases, backups) and its
 * observability pages. Every list shows only the project's resources and
 * links once to the server-wide list for admins who need everything.
 */

import { lazy, useEffect, type ReactNode } from 'react'
import { Link, useSearchParams } from 'react-router'
import {
  Database,
  DatabaseBackup,
  GitBranch,
  Globe,
  RefreshCw,
  type LucideIcon,
} from 'lucide-react'
import type { ProjectGroupResponse } from '@/api/client/types.gen'
import { RecordLink, RecordRow } from '@temps-sdk/ds'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { EmptyState } from '@/components/ui/empty-state'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { ServiceLogo } from '@/components/ui/service-logo'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { DeploymentStatusBadge } from '@/components/deployment/DeploymentStatusBadge'
import { CreateServiceButton } from '@/components/storage/CreateServiceButton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import {
  ServiceScopeProvider,
  scopedServiceId,
  type ServiceScope,
} from '@/contexts/ServiceScopeContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { SERVER_WIDE_LISTS } from '@/lib/console-scope'
import { GROUP_FANOUT_LIMIT } from '@/lib/group-scoped-lists'
import { formatBytesDecimal } from '@/lib/server-monitoring'
import { projectRoot } from '@/lib/project-navigation'
import { groupHref } from '@/lib/terminology'
import {
  useGroupBackups,
  useGroupDatabases,
  useGroupDeployments,
  useGroupDomains,
  type ScopedListState,
} from './useGroupResources'

const GlobalLogs = lazy(() => import('@/pages/observability/GlobalLogs'))
const GlobalErrors = lazy(() => import('@/pages/observability/GlobalErrors'))
const GlobalTraces = lazy(() => import('@/pages/observability/GlobalTraces'))
const GlobalAnalytics = lazy(
  () => import('@/pages/observability/GlobalAnalytics')
)

function useGroupPageChrome(group: ProjectGroupResponse, title: string) {
  usePageTitle(`${title} · ${group.name}`)
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: group.name, href: groupHref(group.slug) },
      { label: title },
    ])
  }, [group.name, group.slug, setBreadcrumbs, title])
}

function ServerWideButton({ to, children }: { to: string; children: string }) {
  return (
    <Button asChild variant="outline" size="sm">
      <Link to={to}>{children}</Link>
    </Button>
  )
}

/**
 * Loading, failure, partial and empty states shared by the lists. An empty
 * result is only "nothing here yet" when every request succeeded and none
 * was left out; otherwise it says the list could not be read in full.
 */
export function ScopedList({
  state,
  isEmpty,
  noun,
  icon,
  emptyTitle,
  emptyDescription,
  emptyAction,
  fanoutOf = 'services',
  children,
}: {
  state: ScopedListState
  isEmpty: boolean
  /** Plural, lower case: "domains". */
  noun: string
  icon: LucideIcon
  emptyTitle: string
  emptyDescription: string
  emptyAction?: ReactNode
  /** What the fan-out is capped on, for the truncation notice. */
  fanoutOf?: string
  children: ReactNode
}) {
  // Rows already loaded stay visible while other requests load or retry.
  if (state.loading && isEmpty) {
    return (
      <div
        className="space-y-2 rounded-lg border p-4"
        role="status"
        aria-label={`Loading ${noun}`}
      >
        {Array.from({ length: 4 }, (_, index) => (
          <Skeleton key={index} className="h-10 w-full" />
        ))}
      </div>
    )
  }
  const incomplete = state.failed > 0 || state.truncated
  if (isEmpty && !incomplete) {
    return (
      <div className="rounded-lg border bg-card">
        <EmptyState
          size="compact"
          icon={icon}
          title={emptyTitle}
          description={emptyDescription}
          action={emptyAction}
        />
      </div>
    )
  }
  const partial = incomplete ? (
    <Alert variant={state.failed > 0 ? 'destructive' : 'default'}>
      <AlertTitle>
        {isEmpty
          ? `This project's ${noun} could not be read in full`
          : 'This list may be incomplete'}
      </AlertTitle>
      <AlertDescription>
        <p>
          {state.failed > 0
            ? `${state.failed} ${state.failed === 1 ? 'request' : 'requests'} for this project's ${noun} failed.`
            : `Only the first ${GROUP_FANOUT_LIMIT} ${fanoutOf} are included${isEmpty ? `, and they have no ${noun}` : ''}.`}
        </p>
        {state.failed > 0 && (
          <Button
            variant="outline"
            size="sm"
            className="mt-3"
            onClick={state.refetch}
          >
            <RefreshCw className="size-4" />
            Retry {noun}
          </Button>
        )}
      </AlertDescription>
    </Alert>
  ) : null
  if (isEmpty) return partial
  return (
    <div className="space-y-4">
      {partial}
      {children}
    </div>
  )
}

function ServiceLink({
  slug,
  name,
  sub,
}: {
  slug: string
  name: string
  sub?: string
}) {
  return (
    <Link
      to={sub ? `${projectRoot(slug)}/${sub}` : projectRoot(slug)}
      className="hover:underline"
    >
      {name}
    </Link>
  )
}

export function GroupDeploymentsPage({
  group,
}: {
  group: ProjectGroupResponse
}) {
  useGroupPageChrome(group, 'Deployments')
  const { deployments, ...state } = useGroupDeployments(group.projects)
  return (
    <PageContainer innerClassName="space-y-6">
      <PageHeader
        title="Deployments"
        description="Recent deployments of this project's services."
      />
      <ScopedList
        state={state}
        isEmpty={deployments.length === 0}
        noun="deployments"
        icon={GitBranch}
        emptyTitle="No deployments in this project yet"
        emptyDescription="Deploy one of its services to see it here."
      >
        <div className="@container rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Service</TableHead>
                <TableHead>Status</TableHead>
                <TableHead className="hidden @xl:table-cell">
                  Environment
                </TableHead>
                <TableHead className="hidden @2xl:table-cell">Commit</TableHead>
                <TableHead className="text-right">Started</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {deployments.map((deployment) => (
                <RecordRow key={`${deployment.service.id}-${deployment.id}`}>
                  <TableCell className="font-medium [overflow-wrap:anywhere]">
                    <RecordLink
                      to={`${projectRoot(deployment.service.slug)}/deployments/${deployment.id}`}
                    >
                      {deployment.service.name} #{deployment.id}
                    </RecordLink>
                    <span className="block text-xs text-muted-foreground @xl:hidden">
                      {deployment.environment.name}
                    </span>
                  </TableCell>
                  <TableCell>
                    <DeploymentStatusBadge deployment={deployment} />
                  </TableCell>
                  <TableCell className="hidden @xl:table-cell">
                    {deployment.environment.name}
                  </TableCell>
                  <TableCell className="hidden max-w-64 truncate text-muted-foreground @2xl:table-cell">
                    {deployment.commit_message ?? deployment.tag ?? '—'}
                  </TableCell>
                  <TableCell className="text-right text-muted-foreground">
                    <TimeAgo date={deployment.created_at} />
                  </TableCell>
                </RecordRow>
              ))}
            </TableBody>
          </Table>
        </div>
        <p className="text-xs text-muted-foreground">
          The latest deployments of each service. A service&apos;s full history
          is on its Deployments page.
        </p>
      </ScopedList>
    </PageContainer>
  )
}

export function GroupDomainsPage({ group }: { group: ProjectGroupResponse }) {
  useGroupPageChrome(group, 'Domains')
  const { domains, ...state } = useGroupDomains(group.projects)
  const first = group.projects[0]
  return (
    <PageContainer innerClassName="space-y-6">
      <PageHeader
        title="Domains"
        description="Custom domains of this project's services."
        actions={
          <ServerWideButton to={SERVER_WIDE_LISTS.domains.url}>
            {SERVER_WIDE_LISTS.domains.title}
          </ServerWideButton>
        }
      />
      <ScopedList
        state={state}
        isEmpty={domains.length === 0}
        noun="domains"
        icon={Globe}
        emptyTitle="No custom domains in this project yet"
        emptyDescription="Domains are added to a service, per environment."
        emptyAction={
          first ? (
            <Button asChild variant="outline">
              <Link to={`${projectRoot(first.slug)}/domains`}>
                Add a domain to {first.name}
              </Link>
            </Button>
          ) : undefined
        }
      >
        <div className="@container rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Domain</TableHead>
                <TableHead className="hidden @xl:table-cell">Service</TableHead>
                <TableHead className="hidden @2xl:table-cell">
                  Environment
                </TableHead>
                <TableHead className="text-right">Status</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {domains.map((domain) => (
                <RecordRow key={domain.id}>
                  <TableCell className="font-medium [overflow-wrap:anywhere]">
                    <RecordLink
                      to={`${projectRoot(domain.service.slug)}/domains`}
                    >
                      {domain.domain}
                    </RecordLink>
                    <span className="block text-xs text-muted-foreground @xl:hidden">
                      {domain.service.name}
                    </span>
                  </TableCell>
                  <TableCell className="hidden @xl:table-cell">
                    <ServiceLink
                      slug={domain.service.slug}
                      name={domain.service.name}
                    />
                  </TableCell>
                  <TableCell className="hidden @2xl:table-cell">
                    {domain.environment?.name ?? '—'}
                  </TableCell>
                  <TableCell className="text-right">
                    <Badge variant="outline">{domain.status}</Badge>
                  </TableCell>
                </RecordRow>
              ))}
            </TableBody>
          </Table>
        </div>
      </ScopedList>
    </PageContainer>
  )
}

export function GroupDatabasesPage({ group }: { group: ProjectGroupResponse }) {
  useGroupPageChrome(group, 'Databases')
  const { databases, ...state } = useGroupDatabases(group.projects)
  return (
    <PageContainer innerClassName="space-y-6">
      <PageHeader
        title="Databases"
        description="Databases linked to this project's services."
        actions={
          <div className="flex flex-wrap items-center gap-2">
            <ServerWideButton to={SERVER_WIDE_LISTS.databases.url}>
              {SERVER_WIDE_LISTS.databases.title}
            </ServerWideButton>
            <CreateServiceButton label="Create database" />
          </div>
        }
      />
      <ScopedList
        state={state}
        isEmpty={databases.length === 0}
        noun="databases"
        icon={Database}
        emptyTitle="No databases linked to this project's services yet"
        emptyDescription="Create one here, or link an existing one from a service's Databases page."
      >
        <div className="@container rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Database</TableHead>
                <TableHead className="hidden @xl:table-cell">Used by</TableHead>
                <TableHead className="text-right">Status</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {databases.map((db) => (
                <RecordRow key={db.id}>
                  <TableCell className="font-medium">
                    <span className="flex items-center gap-2">
                      <ServiceLogo
                        service={db.type}
                        className="size-4 shrink-0"
                      />
                      <RecordLink to={`/storage/${db.id}`}>
                        {db.name}
                      </RecordLink>
                    </span>
                    <span className="block text-xs text-muted-foreground @xl:hidden">
                      Used by {db.usedBy.map((m) => m.name).join(', ')}
                    </span>
                  </TableCell>
                  <TableCell className="hidden @xl:table-cell">
                    {db.usedBy.map((member, index) => (
                      <span key={member.id}>
                        {index > 0 && ', '}
                        <ServiceLink
                          slug={member.slug}
                          name={member.name}
                          sub="storage"
                        />
                      </span>
                    ))}
                  </TableCell>
                  <TableCell className="text-right">
                    <Badge variant="outline">{db.status}</Badge>
                  </TableCell>
                </RecordRow>
              ))}
            </TableBody>
          </Table>
        </div>
      </ScopedList>
    </PageContainer>
  )
}

export function GroupBackupsPage({ group }: { group: ProjectGroupResponse }) {
  useGroupPageChrome(group, 'Backups')
  const { databases, ...databaseState } = useGroupDatabases(group.projects)
  const { backups, ...backupState } = useGroupBackups(databases)
  const state: ScopedListState = {
    loading: databaseState.loading || backupState.loading,
    failed: databaseState.failed + backupState.failed,
    truncated: databaseState.truncated || backupState.truncated,
    refetch: () => {
      databaseState.refetch()
      backupState.refetch()
    },
  }
  return (
    <PageContainer innerClassName="space-y-6">
      <PageHeader
        title="Backups"
        description="Recent backups of the databases this project uses."
        actions={
          <ServerWideButton to={SERVER_WIDE_LISTS.backups.url}>
            {SERVER_WIDE_LISTS.backups.title}
          </ServerWideButton>
        }
      />
      <ScopedList
        state={state}
        isEmpty={backups.length === 0}
        noun="backups"
        fanoutOf="services or databases"
        icon={DatabaseBackup}
        emptyTitle={
          databases.length === 0
            ? "This project's services use no databases yet"
            : "No backups of this project's databases yet"
        }
        emptyDescription="Backups are scheduled per database, from its page."
      >
        <div className="@container rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Database</TableHead>
                <TableHead>State</TableHead>
                <TableHead className="hidden @xl:table-cell">
                  Destination
                </TableHead>
                <TableHead className="hidden @2xl:table-cell">Size</TableHead>
                <TableHead className="text-right">Started</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {backups.map((backup) => (
                <RecordRow key={`${backup.database.id}-${backup.id}`}>
                  <TableCell className="font-medium [overflow-wrap:anywhere]">
                    <RecordLink to={`/storage/${backup.database.id}`}>
                      {backup.database.name}
                    </RecordLink>
                    <span className="block text-xs text-muted-foreground @xl:hidden">
                      {backup.s3_source_name}
                    </span>
                  </TableCell>
                  <TableCell>
                    <Badge variant="outline">{backup.state}</Badge>
                  </TableCell>
                  <TableCell className="hidden @xl:table-cell">
                    {backup.s3_source_name}
                  </TableCell>
                  <TableCell className="hidden @2xl:table-cell">
                    {formatBytesDecimal(backup.size_bytes)}
                  </TableCell>
                  <TableCell className="text-right text-muted-foreground">
                    <TimeAgo date={backup.started_at} />
                  </TableCell>
                </RecordRow>
              ))}
            </TableBody>
          </Table>
        </div>
      </ScopedList>
    </PageContainer>
  )
}

export type GroupObserveKind = 'logs' | 'errors' | 'traces' | 'analytics'

const OBSERVE_PAGES = {
  logs: GlobalLogs,
  errors: GlobalErrors,
  traces: GlobalTraces,
  analytics: GlobalAnalytics,
} as const

/**
 * A server-wide observability page narrowed to the project's services.
 * Logs takes several services at once, so it starts on all of them; the
 * others take one, so they start on the first service and offer the rest.
 */
export function GroupObservePage({
  group,
  kind,
}: {
  group: ProjectGroupResponse
  kind: GroupObserveKind
}) {
  const [params, setParams] = useSearchParams()
  const requestedRaw = params.get('project_id')
  const requested = requestedRaw ? Number(requestedRaw) : undefined
  const scope: ServiceScope = {
    label: group.name,
    href: groupHref(group.slug),
    services: group.projects.map(({ id, name, slug }) => ({ id, name, slug })),
    serverWide: SERVER_WIDE_LISTS[kind],
  }
  const effective = scopedServiceId(scope, requested, {
    allowAll: kind === 'logs',
  })
  const needsCorrection =
    (effective === undefined ? null : String(effective)) !== requestedRaw
  useEffect(() => {
    if (!needsCorrection) return
    setParams(
      (current) => {
        const next = new URLSearchParams(current)
        if (effective === undefined) next.delete('project_id')
        else next.set('project_id', String(effective))
        return next
      },
      { replace: true }
    )
  }, [effective, needsCorrection, setParams])
  if (group.projects.length === 0) {
    return (
      <PageContainer>
        <EmptyState
          size="compact"
          icon={Database}
          title="This project has no services yet"
          description="Add a service to see its data here."
          action={
            <Button asChild variant="outline">
              <Link to={SERVER_WIDE_LISTS[kind].url}>
                {SERVER_WIDE_LISTS[kind].title}
              </Link>
            </Button>
          }
        />
      </PageContainer>
    )
  }
  // Never render the page with a service outside the project, not even for
  // the one render before the URL is corrected.
  if (needsCorrection) return null
  const Page = OBSERVE_PAGES[kind]
  return (
    <ServiceScopeProvider value={scope}>
      <Page />
    </ServiceScopeProvider>
  )
}
