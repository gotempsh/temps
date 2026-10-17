// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useEffect, useMemo, useState } from 'react'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useDashboardAnalytics } from '@/hooks/useDashboardAnalytics'
import { useDashboardHealth } from '@/hooks/useDashboardHealth'
import { useProjectsMonitorHealth } from '@/hooks/useProjectsMonitorHealth'
import { useLatestDeploymentMedia } from '@/hooks/useLatestDeploymentMedia'
import { usePageTitle } from '@/hooks/usePageTitle'
import { FirstProjectOnboarding } from '@/components/dashboard/FirstProjectOnboarding'
import { SIMULATE_EMPTY_INSTALL } from '@/lib/devSimulate'
import { markReadFailureHandled } from '@/lib/route-query-failures'
import { ProjectCard } from '@/components/dashboard/ProjectCard'
import { OnboardingNextStepCard } from '@/components/dashboard/OnboardingNextStepCard'
import { WorkerNodeRequiredBanner } from '@/components/nodes/WorkerNodeRequiredBanner'
import { ProjectCardSkeleton } from '@/components/skeletons/ProjectCardSkeleton'
import { Button } from '@/components/ui/button'
import { CreateActionButton } from '@/components/ui/create-action-button'
import { ListToolbar } from '@/components/layout/ListToolbar'
import { Alert, AlertTitle, AlertDescription } from '@/components/ui/alert'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { getProjectsOptions } from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'
import { subDays } from 'date-fns'
import {
  AlertTriangle,
  ArrowRight,
  Boxes,
  Download,
  ExternalLink,
  RefreshCw,
  X,
} from 'lucide-react'
import { Link } from 'react-router'
import { useComposedSearchParams } from '@temps-sdk/ds'
import { SourceLogo } from '@/components/imports/SourceLogo'
import {
  TOP_MIGRATION_SOURCES,
  importHref,
} from '@/components/imports/migration-sources'
import { CreateProjectGroupDialog } from '@/components/project-groups/CreateProjectGroupDialog'
import { ProjectGroupCard } from '@/components/project-groups/ProjectGroupCard'
import { projectListItems } from '@/components/project-groups/project-list-items'
import { useProjectGroups } from '@/components/project-groups/useProjectGroups'
import type { ProjectGroupResponse, ProjectResponse } from '@/api/client'
import {
  PROJECT_PAGE_SIZE_OPTIONS,
  projectPageCount,
  readProjectPagination,
  withProjectPagination,
  withProjectSearch,
} from '@/lib/project-list-pagination'
import { serviceCount } from '@/lib/terminology'

/** `/projects?new=project` opens the new-project dialog (command palette). */
const CREATE_PARAM = 'new'
const RENAME_NOTICE_KEY = 'temps.servicesRenameNoticeDismissed'
const PROJECTS_DOCS_URL = 'https://temps.sh/docs/concepts/projects'

function groupMatches(group: ProjectGroupResponse, needle: string): boolean {
  return (
    group.name.toLowerCase().includes(needle) ||
    group.slug.toLowerCase().includes(needle) ||
    group.projects.some(
      (member) =>
        member.name.toLowerCase().includes(needle) ||
        member.slug.toLowerCase().includes(needle)
    )
  )
}

export function Projects() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [searchParams, updateSearchParams] = useComposedSearchParams()
  const { page, pageSize } = readProjectPagination(searchParams)
  const projectSearch = searchParams.get('q') ?? ''
  const setProjectSearch = (value: string) => {
    updateSearchParams((current) => withProjectSearch(current, value))
  }
  const normalizedProjectSearch = projectSearch.trim().toLowerCase()
  const [creatingGroup, setCreatingGroup] = useState(false)
  const [renameNoticeDismissed, setRenameNoticeDismissed] = useState(
    readRenameNoticeDismissed
  )
  const dismissRenameNotice = () => {
    setRenameNoticeDismissed(true)
    try {
      window.localStorage.setItem(RENAME_NOTICE_KEY, '1')
    } catch {
      // Private mode: hidden for this page only.
    }
  }

  // Opened from elsewhere (the command palette's "New project"): the request
  // lives in the URL, so the dialog is open while it is there and the
  // parameter is dropped when the dialog closes.
  const createRequested = searchParams.get(CREATE_PARAM) === 'project'
  const createDialogOpen = creatingGroup || createRequested
  const setCreateDialogOpen = (open: boolean) => {
    setCreatingGroup(open)
    if (open || !createRequested) return
    updateSearchParams(
      (current) => {
        const next = new URLSearchParams(current)
        next.delete(CREATE_PARAM)
        return next
      },
      { replace: true }
    )
  }

  const groupsQuery = useProjectGroups()
  const groupsLoaded = groupsQuery.isSuccess
  const groups = groupsQuery.groups
  const visibleAllGroups = useMemo(
    () => (SIMULATE_EMPTY_INSTALL ? [] : groups),
    [groups]
  )
  // With at least one project (group), its services show inside its card
  // and only the rest are listed (and paged) on their own.
  const grouped = groupsLoaded && groups.length > 0

  const {
    data: rawProjectsData,
    isLoading: servicesLoading,
    isError,
    error: servicesError,
    isFetching,
    refetch,
  } = useQuery({
    ...getProjectsOptions({
      query: {
        page,
        per_page: pageSize,
        search: normalizedProjectSearch || undefined,
        ungrouped: grouped ? true : undefined,
      },
    }),
    // Which list to ask for depends on the groups: wait for them rather than
    // render one layout and swap it for another. If they fail, every
    // service is listed, with the failure said above it. Once they have
    // failed a first time they are being retried, so the list is asked for
    // alongside the retries instead of after them: the two outages are one
    // wait, not two.
    enabled: !groupsQuery.isLoading || groupsQuery.failureCount > 0,
  })

  // TEMP: force an empty (brand-new install) dashboard while iterating on the
  // first-run experience. See lib/devSimulate.ts.
  const projectsData = SIMULATE_EMPTY_INSTALL
    ? ({ ...rawProjectsData, projects: [], total: 0 } as typeof rawProjectsData)
    : rawProjectsData
  const standaloneTotal = projectsData?.total ?? 0
  const totalPages = projectPageCount(standaloneTotal, pageSize)
  const isPageOutOfRange = Boolean(standaloneTotal) && page > totalPages

  const setPagination = useCallback(
    (nextPage: number, nextPageSize = pageSize, replace = false) => {
      updateSearchParams(
        (current) =>
          withProjectPagination(current, {
            page: nextPage,
            pageSize: nextPageSize,
          }),
        { replace }
      )
    },
    [pageSize, updateSearchParams]
  )

  useEffect(() => {
    const hasCanonicalPage = searchParams.get('page') === String(page)
    const hasCanonicalPageSize =
      searchParams.get('page_size') === String(pageSize)

    if (!hasCanonicalPage || !hasCanonicalPageSize) {
      setPagination(page, pageSize, true)
    }
  }, [page, pageSize, searchParams, setPagination])

  useEffect(() => {
    if (isPageOutOfRange) setPagination(totalPages, pageSize, true)
  }, [isPageOutOfRange, pageSize, setPagination, totalPages])

  // Both lists explain their own failure below, with their own Retry, so the
  // route-level report has nothing to add.
  const groupsError = groupsQuery.isError ? groupsQuery.error : null
  useEffect(() => {
    markReadFailureHandled(groupsError)
  }, [groupsError])
  const servicesFailure = isError ? servicesError : null
  useEffect(() => {
    markReadFailureHandled(servicesFailure)
  }, [servicesFailure])

  // The server filters services; projects are filtered here by their own
  // name or any of their services' names.
  const visibleGroups = useMemo(
    () =>
      visibleAllGroups.filter(
        (group) =>
          !normalizedProjectSearch ||
          groupMatches(group, normalizedProjectSearch)
      ),
    [normalizedProjectSearch, visibleAllGroups]
  )
  const standaloneProjects = useMemo(
    () => projectsData?.projects ?? [],
    [projectsData?.projects]
  )
  // Projects sit on the first page, ordered among its services.
  const listItems = useMemo(
    () =>
      projectListItems(
        grouped && page === 1 ? visibleGroups : [],
        standaloneProjects
      ),
    [grouped, page, standaloneProjects, visibleGroups]
  )

  const memberTotal = useMemo(
    () => visibleAllGroups.reduce((n, g) => n + g.projects.length, 0),
    [visibleAllGroups]
  )
  // Every service the caller can see (without a filter, which is when it
  // decides between onboarding and the list).
  const serviceTotal = standaloneTotal + (grouped ? memberTotal : 0)
  // A failed projects request must not hide first-run onboarding: with no
  // services and no way to learn about projects, the page would otherwise
  // be an alert above a blank list. The failure is still said above it.
  const isEmptyInstall =
    !normalizedProjectSearch &&
    projectsData?.total === 0 &&
    (groupsQuery.isError || (groupsLoaded && visibleAllGroups.length === 0))

  useEffect(() => {
    setBreadcrumbs([{ label: 'Projects' }])
  }, [setBreadcrumbs])

  usePageTitle('Projects')

  // Batch fetch analytics for the services shown as their own card.
  const { startDate, endDate } = useMemo(() => {
    return {
      startDate: subDays(new Date(), 1).toISOString(),
      endDate: new Date().toISOString(),
    }
  }, [])

  const projectIds = useMemo(
    () => standaloneProjects.map((project) => project.id),
    [standaloneProjects]
  )

  const dashboardAnalytics = useDashboardAnalytics(
    projectIds,
    startDate,
    endDate
  )

  // External (telemetry-only) projects have no deployments to report on.
  const hostedProjectIds = useMemo(
    () =>
      standaloneProjects
        .filter((project) => project.source_type !== 'external')
        .map((project) => project.id),
    [standaloneProjects]
  )
  const dashboardHealth = useDashboardHealth(
    hostedProjectIds,
    startDate,
    endDate
  )
  // Uptime monitors answer for services that simply had no visitors, which
  // traffic-derived health cannot. See project-card-health.ts.
  const monitorHealth = useProjectsMonitorHealth(projectIds)
  const latestDeploymentMedia = useLatestDeploymentMedia(hostedProjectIds)

  const renderProjectCard = (project: ProjectResponse) => (
    <ProjectCard
      key={project.id}
      project={project}
      layout="compact"
      analytics={dashboardAnalytics.data?.projects?.[String(project.id)]}
      analyticsLoading={dashboardAnalytics.isLoading}
      analyticsError={dashboardAnalytics.isError}
      healthLoading={dashboardHealth.isLoading}
      healthError={dashboardHealth.isError}
      health={dashboardHealth.data?.projects?.[String(project.id)]}
      monitorHealth={monitorHealth.data?.projects?.[String(project.id)]}
      latestDeploymentMedia={
        latestDeploymentMedia.data?.projects?.[String(project.id)]
      }
      latestDeploymentMediaLoading={latestDeploymentMedia.isLoading}
      latestDeploymentMediaError={
        latestDeploymentMedia.isError && !latestDeploymentMedia.data
      }
    />
  )

  const summary = listSummary({
    loaded: Boolean(projectsData),
    failed: isError,
    searching: Boolean(normalizedProjectSearch),
    grouped,
    groupCount: grouped ? visibleGroups.length : 0,
    standaloneTotal,
    serviceTotal,
  })

  const isLoading = groupsQuery.isLoading || servicesLoading || isPageOutOfRange

  // Announcements and the onboarding checklist are secondary to the list.
  // On a phone each one is a full card of its own, so stacked above the list
  // they push every project off the first screen: below `md` they follow the
  // list (and its pagination) instead. That reorder is CSS-only (`order` in
  // the flex column below), so each card mounts exactly once and keeps its
  // state (e.g. the selected onboarding step) across a rotate or resize.
  const groupingHint = !grouped &&
    groupsLoaded &&
    renameNoticeDismissed &&
    !normalizedProjectSearch &&
    standaloneTotal >= 2 && (
      <GroupingHint onCreate={() => setCreatingGroup(true)} />
    )
  const notices = serviceTotal > 0 && (
    <div className="flex flex-col gap-4 sm:gap-6 max-md:order-last">
      {!renameNoticeDismissed && (
        <ServicesRenameNotice
          onCreateProject={() => setCreatingGroup(true)}
          onDismiss={dismissRenameNotice}
        />
      )}

      <OnboardingNextStepCard />
    </div>
  )

  return (
    <PageContainer
      className="max-sm:py-4"
      innerClassName="flex flex-col gap-4 space-y-0 sm:gap-6"
    >
      {/* Header */}
      <ProjectsHeader
        firstProject={!isLoading && !isError && isEmptyInstall}
        actions={
          <>
            {serviceTotal > 0 && <PlatformStrip />}
            <Button
              variant="outline"
              className="max-sm:flex-1"
              onClick={() => setCreatingGroup(true)}
            >
              <Boxes className="size-4" />
              New project
            </Button>
            <CreateActionButton
              to="/projects/new"
              label="Create service"
              className="max-sm:flex-1"
            />
            {serviceTotal > 0 && <PlatformMenuLink />}
          </>
        }
      />
      <CreateProjectGroupDialog
        open={createDialogOpen}
        onOpenChange={setCreateDialogOpen}
      />

      {/* Nothing here can be built or deployed until something can run it. */}
      <WorkerNodeRequiredBanner />

      {notices}

      {groupsQuery.isError && (
        <div
          role="alert"
          className="flex flex-col gap-2 rounded-lg border border-destructive/40 px-4 py-3 text-sm sm:flex-row sm:items-center"
        >
          <AlertTriangle className="size-4 shrink-0 text-destructive" />
          <p className="flex-1">
            Projects could not be loaded, so every service is listed on its own
            below.
          </p>
          <Button
            variant="outline"
            size="sm"
            disabled={groupsQuery.isFetching}
            onClick={() => void groupsQuery.refetch()}
          >
            <RefreshCw
              className={
                groupsQuery.isFetching ? 'size-4 animate-spin' : 'size-4'
              }
            />
            Retry
          </Button>
        </div>
      )}

      {(serviceTotal > 0 || visibleAllGroups.length > 0 || projectSearch) && (
        <ListToolbar
          searchLabel="Filter projects and services by name or slug"
          placeholder="Filter projects and services…"
          value={projectSearch}
          onChange={setProjectSearch}
          summary={summary}
        />
      )}

      {groupsQuery.truncated && (
        <p className="text-xs text-muted-foreground">
          Showing the first {groups.length} of {groupsQuery.data?.total}{' '}
          projects.
        </p>
      )}

      {isError && (
        <Alert variant="destructive">
          <AlertTitle>Services could not be loaded</AlertTitle>
          <AlertDescription>
            <p>
              {projectsData
                ? 'Showing previously loaded services. Refresh to get the latest status.'
                : 'Check your connection and retry loading your services.'}
            </p>
            <Button
              variant="outline"
              size="sm"
              className="mt-3"
              disabled={isFetching}
              onClick={() => void refetch()}
            >
              <RefreshCw
                className={isFetching ? 'size-4 animate-spin' : 'size-4'}
              />
              {isFetching ? 'Retrying…' : 'Retry loading services'}
            </Button>
          </AlertDescription>
        </Alert>
      )}

      {isLoading ? (
        <div
          role="status"
          className="grid gap-3 sm:grid-cols-2 sm:gap-4 xl:grid-cols-3"
          aria-label="Loading projects and services"
          aria-busy="true"
        >
          {Array.from({ length: pageSize }).map((_, i) => (
            <ProjectCardSkeleton key={i} layout="compact" />
          ))}
        </div>
      ) : isError && !projectsData ? null : isEmptyInstall ? (
        // First-run entry point: no services and no projects. Choose a
        // deployment or migration before asking for credentials. The creation
        // flow owns configuration. An empty project still shows as a card.
        <FirstProjectOnboarding />
      ) : listItems.length === 0 ? (
        normalizedProjectSearch ? (
          <div className="rounded-xl border border-dashed px-6 py-12 text-center">
            <p className="font-medium">No matching projects or services</p>
            <p className="mt-1 text-sm text-muted-foreground">
              No project or service name contains “{projectSearch.trim()}”.
            </p>
            <Button
              variant="outline"
              size="sm"
              className="mt-4"
              onClick={() => setProjectSearch('')}
            >
              Clear filter
            </Button>
          </div>
        ) : null
      ) : (
        <>
          {groupingHint && (
            <div className="max-md:order-[9998]">{groupingHint}</div>
          )}
          <div className="grid gap-3 sm:grid-cols-2 sm:gap-4 xl:grid-cols-3">
            {listItems.map((item) =>
              item.kind === 'group' ? (
                <ProjectGroupCard key={item.key} group={item.group} />
              ) : (
                renderProjectCard(item.project)
              )
            )}
          </div>
        </>
      )}

      {/* Pagination of the services listed on their own. */}
      {projectsData && standaloneTotal > 0 && !isPageOutOfRange && (
        <ResponsivePagination
          page={page}
          pageSize={pageSize}
          total={standaloneTotal}
          totalPages={totalPages}
          pageSizeOptions={PROJECT_PAGE_SIZE_OPTIONS}
          ariaLabel="Service list pagination"
          pageSizeAriaLabel="Services per page"
          className="pt-2"
          onPageChange={(nextPage) => setPagination(nextPage)}
          onPageSizeChange={(nextPageSize) => setPagination(1, nextPageSize)}
        />
      )}
    </PageContainer>
  )
}

function listSummary({
  loaded,
  failed,
  searching,
  grouped,
  groupCount,
  standaloneTotal,
  serviceTotal,
}: {
  loaded: boolean
  failed: boolean
  searching: boolean
  grouped: boolean
  groupCount: number
  standaloneTotal: number
  serviceTotal: number
}): string {
  if (!loaded) return failed ? 'Service count unavailable' : 'Loading services…'
  const projects = `${groupCount} ${groupCount === 1 ? 'project' : 'projects'}`
  if (searching) {
    return grouped
      ? `${groupCount} matching ${groupCount === 1 ? 'project' : 'projects'} · ${standaloneTotal} matching ${standaloneTotal === 1 ? 'service' : 'services'} outside them`
      : `${standaloneTotal} matching ${standaloneTotal === 1 ? 'service' : 'services'}`
  }
  if (grouped) {
    return `${projects} · ${serviceCount(serviceTotal)}, ${standaloneTotal} not in a project`
  }
  return serviceCount(standaloneTotal)
}

function readRenameNoticeDismissed(): boolean {
  try {
    return window.localStorage.getItem(RENAME_NOTICE_KEY) === '1'
  } catch {
    return false
  }
}

/**
 * Shown once to installs that had apps before the rename: they are Services
 * now, and a Project is a new, optional grouping of them.
 */
function ServicesRenameNotice({
  onCreateProject,
  onDismiss,
}: {
  onCreateProject: () => void
  onDismiss: () => void
}) {
  return (
    <div className="flex flex-col gap-3 rounded-lg border px-4 py-3 lg:flex-row lg:items-center">
      <div className="flex size-8 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
        <Boxes className="size-4" />
      </div>
      <p className="flex-1 text-sm text-muted-foreground">
        <span className="font-medium text-foreground">
          Your apps are now called Services.
        </span>{' '}
        A Project groups the services that ship together (frontend, API,
        workers) so they share a repository, variables and routes.
      </p>
      <div className="flex flex-wrap items-center gap-2">
        <Button variant="outline" size="sm" onClick={onCreateProject}>
          Create a project
        </Button>
        <Button variant="ghost" size="sm" asChild>
          <a href={PROJECTS_DOCS_URL} target="_blank" rel="noopener noreferrer">
            Learn more
            <ExternalLink className="size-3.5" />
          </a>
        </Button>
        <Button
          variant="ghost"
          size="icon"
          className="size-8"
          aria-label="Dismiss this notice"
          onClick={onDismiss}
        >
          <X className="size-4" />
        </Button>
      </div>
    </div>
  )
}

/**
 * Projects page header. The title block is fixed; `actions` is what the
 * migration-entry-point variants swap out.
 */
function ProjectsHeader({
  actions,
  firstProject = false,
}: {
  actions?: React.ReactNode
  firstProject?: boolean
}) {
  return (
    <PageHeader
      title={firstProject ? 'Deploy your first application' : 'Projects'}
      description={
        firstProject
          ? 'Bring your code. Add a database if you need one. Get a working URL.'
          : 'Your services, grouped into the projects they ship as'
      }
      actions={actions}
    />
  )
}

/**
 * Migration entry point. The platforms themselves are the affordance: brand
 * marks sit inline in the header so someone arriving from Coolify or Dokploy
 * recognises the path instead of reading for it, and each mark deep-links the
 * import wizard with that source already selected — skipping its first step.
 */
function PlatformStrip() {
  return (
    <div className="hidden items-center gap-1 rounded-md border p-1 sm:flex">
      <span className="px-1.5 text-xs text-muted-foreground">Migrate from</span>
      {TOP_MIGRATION_SOURCES.map((p) => (
        <Link
          key={p.source}
          to={importHref(p.source)}
          title={`Import from ${p.label}`}
          aria-label={`Import from ${p.label}`}
          className="rounded p-1.5 transition-colors hover:bg-accent"
        >
          <SourceLogo source={p.source} className="h-4 w-4" />
        </Link>
      ))}
      <Link
        to="/projects/import-wizard"
        className="rounded p-1.5 text-muted-foreground transition-colors hover:bg-accent"
        title="All platforms"
        aria-label="All platforms"
      >
        <ArrowRight className="h-4 w-4" />
      </Link>
    </div>
  )
}

/**
 * Phone form of {@link PlatformStrip}: five brand marks plus a label would
 * take a row of the header on their own, so below `sm` the migration entry
 * point is one icon button on the same row as the create actions. It opens the
 * import wizard, which lists every platform.
 */
function PlatformMenuLink() {
  return (
    <Button
      variant="outline"
      size="icon"
      className="sm:hidden"
      asChild
      title="Migrate from another platform"
    >
      <Link
        to="/projects/import-wizard"
        aria-label="Migrate from another platform"
      >
        <Download className="size-4" />
      </Link>
    </Button>
  )
}

/**
 * Introduces grouping where the services are, before any project exists.
 * Shown only with two or more services, where grouping can do something.
 */
function GroupingHint({ onCreate }: { onCreate: () => void }) {
  return (
    <div className="flex flex-col gap-3 rounded-lg border px-4 py-3 lg:flex-row lg:items-center">
      <div className="flex size-8 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
        <Boxes className="size-4" />
      </div>
      <p className="flex-1 text-sm text-muted-foreground">
        <span className="font-medium text-foreground">
          Services that ship together?
        </span>{' '}
        Group a frontend, API and worker into one project to see them side by
        side and share environment variables between them.
      </p>
      <Button variant="outline" size="sm" onClick={onCreate}>
        Create a project
      </Button>
    </div>
  )
}
