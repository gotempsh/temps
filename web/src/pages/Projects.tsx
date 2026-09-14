// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useEffect, useMemo } from 'react'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useDashboardAnalytics } from '@/hooks/useDashboardAnalytics'
import { useDashboardHealth } from '@/hooks/useDashboardHealth'
import { useProjectsMonitorHealth } from '@/hooks/useProjectsMonitorHealth'
import { useLatestDeploymentMedia } from '@/hooks/useLatestDeploymentMedia'
import { usePageTitle } from '@/hooks/usePageTitle'
import { FirstProjectOnboarding } from '@/components/dashboard/FirstProjectOnboarding'
import { SIMULATE_EMPTY_INSTALL } from '@/lib/devSimulate'
import { ProjectCard } from '@/components/dashboard/ProjectCard'
import { OnboardingNextStepCard } from '@/components/dashboard/OnboardingNextStepCard'
import { ProjectCardSkeleton } from '@/components/skeletons/ProjectCardSkeleton'
import { Button } from '@/components/ui/button'
import { CreateActionButton } from '@/components/ui/create-action-button'
import { ListToolbar } from '@/components/layout/ListToolbar'
import { Alert, AlertTitle, AlertDescription } from '@/components/ui/alert'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import {
  getProjectsOptions,
  listGitProvidersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'
import { subDays } from 'date-fns'
import { ArrowRight, RefreshCw } from 'lucide-react'
import { Link, useSearchParams } from 'react-router'
import { SourceLogo } from '@/components/imports/SourceLogo'
import {
  TOP_MIGRATION_SOURCES,
  importHref,
} from '@/components/imports/migration-sources'
import {
  PROJECT_PAGE_SIZE_OPTIONS,
  projectPageCount,
  readProjectPagination,
  withProjectPagination,
  withProjectSearch,
} from '@/lib/project-list-pagination'

const SEARCH_CATALOG_LIMIT = 50

export function Projects() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [searchParams, setSearchParams] = useSearchParams()
  const { page, pageSize } = readProjectPagination(searchParams)
  const projectSearch = searchParams.get('q') ?? ''
  const setProjectSearch = (value: string) => {
    setSearchParams((current) => withProjectSearch(current, value), {
      replace: true,
    })
  }
  const normalizedProjectSearch = projectSearch.trim().toLowerCase()

  const {
    data: rawProjectsData,
    isLoading,
    isError,
    isFetching,
    refetch,
  } = useQuery({
    ...getProjectsOptions({
      query: {
        page: normalizedProjectSearch ? 1 : page,
        // The list endpoint has no text filter, so search a deliberately
        // bounded catalogue, with the scope disclosed beside the filter.
        per_page: normalizedProjectSearch ? SEARCH_CATALOG_LIMIT : pageSize,
      },
    }),
  })

  const { data: rawGitProviders, isLoading: gitProvidersLoading } = useQuery({
    ...listGitProvidersOptions({}),
    retry: false,
  })

  // TEMP: force an empty (brand-new install) dashboard while iterating on the
  // first-run experience. See lib/devSimulate.ts.
  const projectsData = SIMULATE_EMPTY_INSTALL
    ? ({ ...rawProjectsData, projects: [], total: 0 } as typeof rawProjectsData)
    : rawProjectsData
  const gitProviders = SIMULATE_EMPTY_INSTALL ? [] : rawGitProviders
  const totalPages = projectPageCount(projectsData?.total ?? 0, pageSize)
  const isPageOutOfRange =
    !normalizedProjectSearch &&
    Boolean(projectsData?.total) &&
    page > totalPages

  const setPagination = useCallback(
    (nextPage: number, nextPageSize = pageSize, replace = false) => {
      setSearchParams(
        (current) =>
          withProjectPagination(current, {
            page: nextPage,
            pageSize: nextPageSize,
          }),
        { replace }
      )
    },
    [pageSize, setSearchParams]
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

  const visibleProjects = useMemo(() => {
    const projects = projectsData?.projects ?? []
    if (!normalizedProjectSearch) return projects
    return projects.filter(
      (project) =>
        project.name.toLowerCase().includes(normalizedProjectSearch) ||
        project.slug.toLowerCase().includes(normalizedProjectSearch)
    )
  }, [normalizedProjectSearch, projectsData?.projects])

  useEffect(() => {
    setBreadcrumbs([{ label: 'Projects' }])
  }, [setBreadcrumbs])

  usePageTitle('Projects')

  // Batch fetch analytics for all visible projects
  const { startDate, endDate } = useMemo(() => {
    return {
      startDate: subDays(new Date(), 1).toISOString(),
      endDate: new Date().toISOString(),
    }
  }, [])

  const projectIds = useMemo(
    () => visibleProjects.map((project) => project.id),
    [visibleProjects]
  )

  const dashboardAnalytics = useDashboardAnalytics(
    projectIds,
    startDate,
    endDate
  )

  const dashboardHealth = useDashboardHealth(projectIds, startDate, endDate)
  // Uptime monitors answer for projects that simply had no visitors, which
  // traffic-derived health cannot. See project-card-health.ts.
  const monitorHealth = useProjectsMonitorHealth(projectIds)
  const latestDeploymentMedia = useLatestDeploymentMedia(projectIds)

  const renderProjectCards = () =>
    visibleProjects.map((project) => (
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
    ))

  return (
    <PageContainer innerClassName="space-y-6">
      {/* Header */}
      <ProjectsHeader
        actions={
          <>
            <PlatformStrip />
            <CreateActionButton to="/projects/new" label="New Project" />
          </>
        }
      />

      {(projectsData?.total ?? 0) > 0 && <OnboardingNextStepCard />}

      {((projectsData?.total ?? 0) > 0 || Boolean(projectSearch)) && (
        <ListToolbar
          searchLabel="Filter projects by name or slug"
          placeholder="Filter projects…"
          value={projectSearch}
          onChange={setProjectSearch}
          summary={
            !projectsData
              ? isError
                ? 'Project count unavailable'
                : 'Loading projects…'
              : normalizedProjectSearch
                ? `${visibleProjects.length} matching ${visibleProjects.length === 1 ? 'project' : 'projects'} · searching ${projectsData.projects.length} of ${projectsData.total}`
                : `${projectsData.total} ${projectsData.total === 1 ? 'project' : 'projects'}`
          }
        />
      )}

      {isError && (
        <Alert variant="destructive">
          <AlertTitle>Projects could not be loaded</AlertTitle>
          <AlertDescription>
            <p>
              {projectsData
                ? 'Showing previously loaded projects. Refresh to get the latest status.'
                : 'Check your connection and retry loading your projects.'}
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
              {isFetching ? 'Retrying…' : 'Retry loading projects'}
            </Button>
          </AlertDescription>
        </Alert>
      )}

      {isLoading ||
      (projectsData?.total === 0 && gitProvidersLoading) ||
      isPageOutOfRange ? (
        <div
          className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3"
          aria-label="Loading projects"
          aria-busy="true"
        >
          {Array.from({ length: pageSize }).map((_, i) => (
            <ProjectCardSkeleton key={i} layout="compact" />
          ))}
        </div>
      ) : isError && !projectsData ? null : projectsData?.total === 0 ? (
        // First-run onboarding. The component is context-aware: when a Git
        // provider is already connected it routes straight into the import
        // wizard (skipping the connect step), and it always surfaces the
        // "deploy a project with a database" and CLI paths.
        <FirstProjectOnboarding
          gitConnected={!!gitProviders && gitProviders.length > 0}
        />
      ) : visibleProjects.length === 0 ? (
        <div className="rounded-xl border border-dashed px-6 py-12 text-center">
          <p className="font-medium">No matching projects</p>
          <p className="mt-1 text-sm text-muted-foreground">
            No project name or slug contains “{projectSearch.trim()}”.
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
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
          {renderProjectCards()}
        </div>
      )}

      {/* Pagination - Only show if there are projects */}
      {projectsData &&
        projectsData.total > 0 &&
        !normalizedProjectSearch &&
        !isPageOutOfRange && (
          <ResponsivePagination
            page={page}
            pageSize={pageSize}
            total={projectsData.total}
            totalPages={totalPages}
            pageSizeOptions={PROJECT_PAGE_SIZE_OPTIONS}
            ariaLabel="Project list pagination"
            pageSizeAriaLabel="Projects per page"
            className="pt-2"
            onPageChange={(nextPage) => setPagination(nextPage)}
            onPageSizeChange={(nextPageSize) => setPagination(1, nextPageSize)}
          />
        )}
    </PageContainer>
  )
}

/**
 * Projects page header. The title block is fixed; `actions` is what the
 * migration-entry-point variants swap out.
 */
function ProjectsHeader({ actions }: { actions: React.ReactNode }) {
  return (
    <PageHeader
      title="Projects"
      description="Manage your projects and their settings"
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
    <div className="flex items-center gap-1 rounded-md border p-1">
      {/* The label is the first thing to go when the header wraps on mobile —
          the brand marks still carry the meaning, and every one of them has an
          accessible name. */}
      <span className="hidden px-1.5 text-xs text-muted-foreground sm:inline">
        Migrate from
      </span>
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
