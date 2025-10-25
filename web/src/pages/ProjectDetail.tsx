import {
  getLastDeploymentOptions,
  getProjectBySlugOptions,
  getActiveVisitors2Options,
  getRepositoryByNameOptions,
} from '@/api/client/@tanstack/react-query.gen'
import NotFound from '@/components/global/NotFound'
import { ProjectAnalytics } from '@/components/project/ProjectAnalytics'
import { ProjectDeployments } from '@/components/project/ProjectDeployments'
import { ProjectDetailSidebar } from '@/components/project/ProjectDetailSidebar'
import { ProjectOverview } from '@/components/project/ProjectOverview'
import { ProjectRuntime } from '@/components/project/ProjectRuntime'
import { ProjectSettings } from '@/components/project/ProjectSettings'
import { ProjectSpeedInsights } from '@/components/project/ProjectSpeedInsights'
import { ProjectStorage } from '@/components/project/ProjectStorage'
import { ProjectMonitors } from '@/components/project/ProjectMonitors'
import { MonitorDetail } from '@/components/project/MonitorDetail'
import { ErrorTracking } from '@/components/projects/ErrorTracking'
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar'
import { Badge } from '@/components/ui/badge'
import { buttonVariants } from '@/components/ui/button'
import { Confetti } from '@/components/ui/confetti'
import { Skeleton } from '@/components/ui/skeleton'

import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { cn } from '@/lib/utils'
import { DeploymentDetails } from '@/pages/DeploymentDetails'
import { ErrorEventDetail } from './ErrorEventDetail'
import { ErrorGroupDetail } from './ErrorGroupDetail'
import RequestLogs from './RequestLogs'
import { useQuery } from '@tanstack/react-query'
import { useEffect } from 'react'
import {
  Link,
  Navigate,
  Route,
  Routes,
  useParams,
  useSearchParams,
} from 'react-router-dom'
import { Card, CardContent } from '@/components/ui/card'

export function ProjectDetail() {
  const { slug } = useParams()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [searchParams, setSearchParams] = useSearchParams()

  // Check for confetti query parameter
  const showConfetti = searchParams.get('showConfetti') === 'true'
  const {
    data: project,
    isLoading,
    error,
    refetch,
  } = useQuery({
    ...getProjectBySlugOptions({
      path: {
        slug: slug || '',
      },
    }),
    retry: false,
    enabled: !!slug,
  })

  const {
    data: lastDeployment,
    isLoading: isLoadingLastDeployment,
    refetch: refetchLastDeployment,
  } = useQuery({
    ...getLastDeploymentOptions({
      path: {
        id: project?.id || 0,
      },
    }),
    enabled: !!project?.id,
    refetchInterval: (query) => {
      const data = query.state.data
      // Poll more frequently for active deployments
      if (data && (data.status === 'pending' || data.status === 'building')) {
        return 2500 // 2.5 seconds for active deployments
      }
      // Keep checking periodically for new deployments
      return 10000 // 10 seconds for completed/failed deployments
    },
    // Also refetch when window regains focus
    refetchOnWindowFocus: true,
  })

  // Fetch active visitors count
  const { data: activeVisitorsCount } = useQuery({
    ...getActiveVisitors2Options({
      path: {
        project_id: project?.id || 0,
      },
    }),
    enabled: !!project,
    refetchInterval: 15000, // Refresh every 30 seconds
  })

  // Fetch repository details for clone URL
  const { data: repository } = useQuery({
    ...getRepositoryByNameOptions({
      path: {
        owner: project?.repo_owner || '',
        name: project?.repo_name || '',
      },
    }),
    enabled: !!project?.repo_owner && !!project?.repo_name,
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: project?.slug || 'Project Details' },
    ])
    // Refresh last deployment when component mounts
    if (project?.id) {
      refetchLastDeployment()
    }
    // Remove confetti parameter after showing
    if (showConfetti) {
      const timer = setTimeout(() => {
        searchParams.delete('showConfetti')
        setSearchParams(searchParams)
      }, 500)
      return () => clearTimeout(timer)
    }
  }, [
    setBreadcrumbs,
    project,
    refetchLastDeployment,
    showConfetti,
    searchParams,
    setSearchParams,
  ])

  usePageTitle(project?.name ? `${project.name}` : '')

  if (error?.message?.includes('404') || (!isLoading && !project)) {
    return <NotFound />
  }

  if (error) {
    return (
      <div className="p-6">
        <ErrorAlert
          title="Failed to load project"
          description={
            error instanceof Error
              ? error.message
              : 'An unexpected error occurred'
          }
          retry={() => refetch()}
        />
      </div>
    )
  }

  if (isLoading) {
    return (
      <div className="flex-1">
        <div className="p-0 sm:p-4 space-y-6 md:p-6">
          <div className="p-2 flex flex-col gap-4 mb-6 sm:mb-8 sm:flex-row sm:items-center sm:justify-between">
            <div className="flex items-center gap-4">
              <Skeleton className="h-8 w-8 rounded-full" />
              <div className="flex flex-wrap items-center gap-2 sm:gap-4">
                <Skeleton className="h-8 w-32" />
                <Skeleton className="h-6 w-20" />
              </div>
            </div>
            <div className="flex gap-2">
              <Skeleton className="h-9 w-24" />
              <Skeleton className="h-9 w-24" />
            </div>
          </div>

          <div className="w-full">
            <div className="relative border-b">
              <div className="max-w-screen overflow-hidden">
                <div className="relative flex items-center">
                  <div className="flex-1 overflow-x-auto no-scrollbar">
                    <div className="min-w-full">
                      <Skeleton className="h-10 w-full" />
                    </div>
                  </div>
                </div>
              </div>
            </div>
          </div>

          <div className="p-2">
            <div className="grid grid-cols-1 sm:grid-cols-3 gap-6">
              {Array.from({ length: 3 }).map((_, i) => (
                <Card key={i}>
                  <CardContent className="p-6">
                    <Skeleton className="h-4 w-24 mb-2" />
                    <div className="flex items-baseline gap-2">
                      <Skeleton className="h-8 w-16" />
                      <Skeleton className="h-4 w-32" />
                    </div>
                  </CardContent>
                </Card>
              ))}
            </div>

            <Card className="mt-6">
              <CardContent className="p-6">
                <div className="grid gap-4">
                  <Skeleton className="h-6 w-24" />
                  <Skeleton className="h-4 w-64" />
                  <div className="flex items-center gap-2">
                    <Skeleton className="h-6 w-20" />
                    <Skeleton className="h-4 w-32" />
                  </div>
                </div>
              </CardContent>
            </Card>
          </div>
        </div>
      </div>
    )
  }
  if (!project) {
    return <NotFound />
  }

  return (
    <div className="flex h-[calc(100vh-4rem)] w-full overflow-hidden">
      <Confetti active={showConfetti} duration={4000} particleCount={100} />
      <ProjectDetailSidebar project={project} />
      <div className="flex flex-1 flex-col overflow-hidden">
        <header className="flex h-16 shrink-0 items-center gap-2 border-b px-4">
          <div className="flex flex-1 items-center justify-between gap-4">
            <div className="flex items-center gap-4">
              <Avatar className="size-8">
                <AvatarImage src={`/api/projects/${project.id}/favicon`} />
                <AvatarFallback>{project.name.charAt(0)}</AvatarFallback>
              </Avatar>
              <div className="flex flex-wrap items-center gap-2">
                <h1 className="text-lg font-semibold">{project.name}</h1>
                <Badge
                  variant={project.last_deployment ? 'default' : 'outline'}
                >
                  {project.last_deployment ? 'Deployed' : 'Not deployed'}
                </Badge>
              </div>
            </div>
            <div className="flex items-center gap-2">
              {activeVisitorsCount !== undefined && (
                <div className="flex items-center gap-1.5 px-2.5 py-1.5 bg-muted/30 rounded-full">
                  <div
                    className={`h-2 w-2 rounded-full ${activeVisitorsCount?.active_visitors > 0 ? 'bg-green-500 animate-pulse' : 'bg-gray-400'}`}
                  />
                  <span className="text-sm font-semibold">
                    {activeVisitorsCount?.active_visitors}
                  </span>
                </div>
              )}
              {repository?.clone_url && (
                <Link
                  to={repository.clone_url.replace('.git', '')}
                  target="_blank"
                  rel="noopener noreferrer"
                  className={cn(
                    buttonVariants({
                      variant: 'outline',
                      size: 'sm',
                    })
                  )}
                >
                  Repository
                </Link>
              )}
              {lastDeployment && !isLoadingLastDeployment && (
                <Link
                  to={lastDeployment.url}
                  target="_blank"
                  rel="noopener noreferrer"
                  className={cn(
                    buttonVariants({
                      size: 'sm',
                    })
                  )}
                >
                  Visit
                </Link>
              )}
            </div>
          </div>
        </header>
        <div className="flex-1 overflow-y-auto p-4">
          <Routes>
            <Route index element={<Navigate to="project" replace />} />
            <Route
              path="project"
              element={
                <ProjectOverview
                  project={project}
                  lastDeployment={lastDeployment}
                />
              }
            />
            <Route
              path="deployments"
              element={<ProjectDeployments project={project} />}
            />
            <Route
              path="deployments/:deploymentId"
              element={<DeploymentDetails project={project} />}
            />
            <Route
              path="analytics/*"
              element={<ProjectAnalytics project={project} />}
            />
            <Route
              path="storage"
              element={<ProjectStorage project={project} />}
            />
            <Route
              path="runtime"
              element={<ProjectRuntime project={project} />}
            />
            <Route
              path="settings/*"
              element={<ProjectSettings project={project} refetch={refetch} />}
            />
            <Route
              path="speed"
              element={<ProjectSpeedInsights project={project} />}
            />
            <Route path="logs/*" element={<RequestLogs project={project} />} />
            <Route
              path="monitors"
              element={<ProjectMonitors project={project} />}
            />
            <Route
              path="monitors/:monitorId"
              element={<MonitorDetail project={project} />}
            />
            <Route
              path="errors"
              element={<ErrorTracking project={project} />}
            />
            <Route
              path="errors/:errorGroupId"
              element={<ErrorGroupDetail project={project} />}
            />
            <Route
              path="errors/:errorGroupId/event/:eventId"
              element={<ErrorEventDetail project={project} />}
            />
          </Routes>
        </div>
      </div>
    </div>
  )
}
