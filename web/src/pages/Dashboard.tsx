import {
  getGeneralStatsOptions,
  getProjectsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ExternalConnectivityAlert } from '@/components/alerts/ExternalConnectivityAlert'
import { MetricCard } from '@/components/dashboard/MetricCard'
import { ProjectCard } from '@/components/dashboard/ProjectCard'
import { EmptyPlaceholder } from '@/components/EmptyPlaceholder'
import { ImprovedOnboardingDashboard } from '@/components/onboarding/ImprovedOnboardingDashboard'
import { MetricCardSkeleton } from '@/components/skeletons/MetricCardSkeleton'
import { ProjectCardSkeleton } from '@/components/skeletons/ProjectCardSkeleton'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQuery } from '@tanstack/react-query'
import { subDays } from 'date-fns'
import { DollarSign, Eye, FolderGit2, Plus, Users } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'

const ITEMS_PER_PAGE = 8

export function Dashboard() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [page, setPage] = useState(1)

  const { data: projectsData, isLoading } = useQuery({
    ...getProjectsOptions({
      query: {
        page,
        per_page: ITEMS_PER_PAGE,
      },
    }),
  })

  useEffect(() => {
    setBreadcrumbs([{ label: 'Dashboard' }])
  }, [setBreadcrumbs])

  usePageTitle('Dashboard')

  // Determine onboarding status
  const hasProjects = (projectsData?.projects?.length || 0) > 0
  const { startDate, endDate } = useMemo(() => {
    return {
      startDate: subDays(new Date(), 1).toISOString(),
      endDate: new Date().toISOString(),
    }
  }, [])
  // Fetch general stats
  const generalStatsQuery = useQuery({
    ...getGeneralStatsOptions({
      query: {
        start_date: startDate,
        end_date: endDate,
      },
    }),
    enabled: hasProjects,
  })
  // Fix: gitProvidersData is an array, not an object with providers property

  // Show loading skeleton while initial data is being fetched
  if (isLoading) {
    return (
      <div className="sm:p-8">
        {/* Metric Cards Skeleton */}
        <div className="grid gap-6 md:grid-cols-4 mb-8">
          <MetricCardSkeleton />
          <MetricCardSkeleton />
          <MetricCardSkeleton />
          <MetricCardSkeleton />
        </div>

        {/* Projects Grid Skeleton */}
        <div className="space-y-6">
          <div className="grid gap-6 md:grid-cols-2">
            {Array.from({ length: ITEMS_PER_PAGE }).map((_, i) => (
              <ProjectCardSkeleton key={i} />
            ))}
          </div>

          {/* Pagination Skeleton */}
          <div className="flex items-center justify-center gap-2">
            <div className="h-9 w-20 bg-muted animate-pulse rounded-md" />
            <div className="h-5 w-24 bg-muted animate-pulse rounded" />
            <div className="h-9 w-16 bg-muted animate-pulse rounded-md" />
          </div>
        </div>
      </div>
    )
  }

  // Show onboarding if no projects exist (even if git provider is configured)
  const shouldShowOnboarding = !hasProjects

  if (shouldShowOnboarding) {
    return (
      <div className="sm:p-8">
        <ImprovedOnboardingDashboard />
      </div>
    )
  }

  return (
    <div className="sm:p-8 ">
      {/* External Connectivity Alert */}
      <ExternalConnectivityAlert showInDashboard dismissible />

      <div className="grid gap-6 md:grid-cols-4 mb-8">
        {/* Projects */}
        {generalStatsQuery.isLoading ? (
          <MetricCardSkeleton />
        ) : generalStatsQuery.error ? (
          <MetricCard
            title="Projects"
            value={projectsData?.total || 0}
            change=""
            icon={<FolderGit2 className="h-5 w-5" />}
          />
        ) : (
          <MetricCard
            title="Projects"
            value={generalStatsQuery.data?.total_projects || 0}
            change=""
            icon={<FolderGit2 className="h-5 w-5" />}
          />
        )}

        {/* Visitors */}
        {generalStatsQuery.isLoading ? (
          <MetricCardSkeleton />
        ) : generalStatsQuery.error ? (
          <MetricCard
            title="Visitors"
            value="N/A"
            change=""
            icon={<Users className="h-5 w-5" />}
          />
        ) : (
          <MetricCard
            title="Visitors"
            value={
              generalStatsQuery.data?.total_unique_visitors?.toLocaleString() ||
              0
            }
            change=""
            icon={<Users className="h-5 w-5" />}
          />
        )}

        {/* Page Views */}
        {generalStatsQuery.isLoading ? (
          <MetricCardSkeleton />
        ) : generalStatsQuery.error ? (
          <MetricCard
            title="Page Views"
            value="N/A"
            change=""
            icon={<Eye className="h-5 w-5" />}
          />
        ) : (
          <MetricCard
            title="Page Views"
            value={
              generalStatsQuery.data?.total_page_views?.toLocaleString() || 0
            }
            change=""
            icon={<Eye className="h-5 w-5" />}
          />
        )}

        {/* Revenue - Coming Soon */}
        <div className="relative">
          {generalStatsQuery.isLoading ? (
            <MetricCardSkeleton />
          ) : generalStatsQuery.error ? (
            <MetricCard
              title="Revenue"
              value="$0"
              change=""
              icon={<DollarSign className="h-5 w-5" />}
            />
          ) : (
            <MetricCard
              title="Revenue"
              value={`$102249.71`}
              change=""
              icon={<DollarSign className="h-5 w-5" />}
            />
          )}
          <div className="absolute inset-0 bg-background/80  rounded-lg flex items-center justify-center">
            <Badge variant="secondary" className="text-xs">
              Coming Soon
            </Badge>
          </div>
        </div>
      </div>

      {/* Projects Grid */}
      <div className="space-y-6">
        {isLoading ? (
          <div className="grid gap-6 md:grid-cols-2">
            {Array.from({ length: ITEMS_PER_PAGE }).map((_, i) => (
              <ProjectCardSkeleton key={i} />
            ))}
          </div>
        ) : projectsData?.projects.length === 0 ? (
          <EmptyPlaceholder
            title="No projects found"
            description="You haven't created any projects yet. Start by creating your first project."
            icon={FolderGit2}
          >
            <Button asChild>
              <Link to="/projects/new">
                <Plus className="mr-2 h-4 w-4" />
                Create Project
              </Link>
            </Button>
          </EmptyPlaceholder>
        ) : (
          <>
            <div className="grid gap-6 md:grid-cols-2">
              {projectsData?.projects.map((project: any) => (
                <ProjectCard key={project.id} project={project} />
              ))}
            </div>

            {/* Pagination */}
            {projectsData && (
              <div className="flex items-center justify-center gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setPage((p) => Math.max(1, p - 1))}
                  disabled={page === 1}
                >
                  Previous
                </Button>
                <span className="text-sm text-muted-foreground">
                  Page {page} of{' '}
                  {Math.ceil(projectsData.total / ITEMS_PER_PAGE)}
                </span>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setPage((p) => p + 1)}
                  disabled={
                    page >= Math.ceil(projectsData.total / ITEMS_PER_PAGE)
                  }
                >
                  Next
                </Button>
              </div>
            )}
          </>
        )}
      </div>
    </div>
  )
}
