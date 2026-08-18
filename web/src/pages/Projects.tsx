import { useEffect, useMemo, useRef, useState } from 'react'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useDashboardAnalytics } from '@/hooks/useDashboardAnalytics'
import { useDashboardHealth } from '@/hooks/useDashboardHealth'
import { useLatestDeploymentMedia } from '@/hooks/useLatestDeploymentMedia'
import { usePageTitle } from '@/hooks/usePageTitle'
import { FirstProjectOnboarding } from '@/components/dashboard/FirstProjectOnboarding'
import { SIMULATE_EMPTY_INSTALL } from '@/lib/devSimulate'
import { ProjectCard } from '@/components/dashboard/ProjectCard'
import { OnboardingNextStepCard } from '@/components/dashboard/OnboardingNextStepCard'
import { ProjectCardSkeleton } from '@/components/skeletons/ProjectCardSkeleton'
import { Button } from '@/components/ui/button'
import { CreateActionButton } from '@/components/ui/create-action-button'
import { Input } from '@/components/ui/input'
import {
  getProjectsOptions,
  listGitProvidersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'
import { subDays } from 'date-fns'
import { ArrowRight, Search, X } from 'lucide-react'
import { Link } from 'react-router'
import { SourceLogo } from '@/components/imports/SourceLogo'
import {
  TOP_MIGRATION_SOURCES,
  importHref,
} from '@/components/imports/migration-sources'

const ITEMS_PER_PAGE = 9
const SEARCH_CATALOG_LIMIT = 50
const SEARCH_RESULTS_LIMIT = 18

export function Projects() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [page, setPage] = useState(1)
  const [projectSearch, setProjectSearch] = useState('')
  const normalizedProjectSearch = projectSearch.trim().toLowerCase()

  const { data: rawProjectsData, isLoading } = useQuery({
    ...getProjectsOptions({
      query: {
        page: normalizedProjectSearch ? 1 : page,
        // The list endpoint has no text filter, so search a deliberately
        // bounded catalogue and cap rendered cards below.
        per_page: normalizedProjectSearch
          ? SEARCH_CATALOG_LIMIT
          : ITEMS_PER_PAGE,
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
  const visibleProjects = useMemo(() => {
    const projects = projectsData?.projects ?? []
    if (!normalizedProjectSearch) return projects
    return projects
      .filter(
        (project) =>
          project.name.toLowerCase().includes(normalizedProjectSearch) ||
          project.slug.toLowerCase().includes(normalizedProjectSearch)
      )
      .slice(0, SEARCH_RESULTS_LIMIT)
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
        latestDeploymentMedia={
          latestDeploymentMedia.data?.projects?.[String(project.id)]
        }
      />
    ))

  return (
    <div className="p-4 sm:p-8 space-y-6">
      {/* Header */}
      <ProjectsHeader
        actions={
          <>
            {(projectsData?.projects.length ?? 0) > 0 && (
              <ProjectSearch
                value={projectSearch}
                onChange={setProjectSearch}
              />
            )}
            <PlatformStrip />
            <CreateActionButton to="/projects/new" label="New Project" />
          </>
        }
      />

      {(projectsData?.projects.length ?? 0) > 0 && <OnboardingNextStepCard />}

      {isLoading || gitProvidersLoading ? (
        <div className="overflow-hidden rounded-xl border bg-card divide-y">
          {Array.from({ length: 4 }).map((_, i) => (
            <ProjectCardSkeleton key={i} />
          ))}
        </div>
      ) : projectsData?.projects.length === 0 ? (
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
        projectsData.projects.length > 0 &&
        !normalizedProjectSearch && (
          <div className="flex items-center justify-center gap-2 pt-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => setPage((p) => Math.max(1, p - 1))}
              disabled={page === 1}
            >
              Previous
            </Button>
            <span className="text-sm text-muted-foreground">
              Page {page} of {Math.ceil(projectsData.total / ITEMS_PER_PAGE)}
            </span>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setPage((p) => p + 1)}
              disabled={page >= Math.ceil(projectsData.total / ITEMS_PER_PAGE)}
            >
              Next
            </Button>
          </div>
        )}
    </div>
  )
}

function ProjectSearch({
  value,
  onChange,
}: {
  value: string
  onChange: (value: string) => void
}) {
  const [isExpanded, setIsExpanded] = useState(Boolean(value))
  const inputRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    if (isExpanded) inputRef.current?.focus()
  }, [isExpanded])

  if (!isExpanded) {
    return (
      <Button
        type="button"
        variant="outline"
        size="icon"
        onClick={() => setIsExpanded(true)}
        aria-label="Filter projects"
        aria-expanded={false}
        title="Filter projects"
      >
        <Search />
      </Button>
    )
  }

  return (
    <div className="relative w-full sm:w-64 xl:w-72">
      <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
      <Input
        ref={inputRef}
        value={value}
        onChange={(event) => onChange(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === 'Escape') {
            onChange('')
            setIsExpanded(false)
          }
        }}
        placeholder="Filter projects…"
        aria-label="Filter projects by name or slug"
        className="pl-9 pr-9"
      />
      <button
        type="button"
        onClick={() => {
          onChange('')
          setIsExpanded(false)
        }}
        className="absolute right-2 top-1/2 flex size-6 -translate-y-1/2 items-center justify-center rounded-sm text-muted-foreground hover:bg-muted hover:text-foreground"
        aria-label="Close project filter"
      >
        <X className="size-4" />
      </button>
    </div>
  )
}

/**
 * Projects page header. The title block is fixed; `actions` is what the
 * migration-entry-point variants swap out.
 */
function ProjectsHeader({ actions }: { actions: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-3 sm:flex-row sm:justify-between sm:items-center">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Projects</h1>
        <p className="text-sm text-muted-foreground">
          Manage your projects and their settings
        </p>
      </div>
      <div className="flex flex-1 flex-wrap justify-start gap-2 sm:justify-end">
        {actions}
      </div>
    </div>
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
