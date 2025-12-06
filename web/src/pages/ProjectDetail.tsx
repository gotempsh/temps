import {
  getLastDeploymentOptions,
  getProjectBySlugOptions,
  getActiveVisitors2Options,
  getRepositoryByNameOptions,
  updateProjectSettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import NotFound from '@/components/global/NotFound'
import { ProjectAnalytics } from '@/components/project/ProjectAnalytics'
import { ProjectDeployments } from '@/components/project/ProjectDeployments'
import {
  ProjectDetailSidebar,
  MobileSidebarProvider,
} from '@/components/project/ProjectDetailSidebar'
import { ProjectDetailHeader } from '@/components/project/ProjectDetailHeader'
import { ProjectOverview } from '@/components/project/ProjectOverview'
import { ProjectRuntime } from '@/components/project/ProjectRuntime'
import { ProjectSettings } from '@/components/project/ProjectSettings'
import { ProjectSpeedInsights } from '@/components/project/ProjectSpeedInsights'
import { ProjectStorage } from '@/components/project/ProjectStorage'
import { ProjectMonitors } from '@/components/project/ProjectMonitors'
import { MonitorDetail } from '@/components/project/MonitorDetail'
import { ErrorTracking } from '@/components/projects/ErrorTracking'
import { EnvironmentsTabsView } from './EnvironmentsTabsView'
import { Confetti } from '@/components/ui/confetti'
import { Skeleton } from '@/components/ui/skeleton'

import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { DeploymentDetails } from '@/pages/DeploymentDetails'
import { ErrorEventDetail } from './ErrorEventDetail'
import { ErrorGroupDetail } from './ErrorGroupDetail'
import RequestLogs from './RequestLogs'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { useEffect } from 'react'
import {
  Navigate,
  Route,
  Routes,
  useParams,
  useSearchParams,
} from 'react-router-dom'
import { Card, CardContent } from '@/components/ui/card'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { ShieldAlert } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { toast } from 'sonner'

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
      if (data && (data.status === 'pending' || data.status === 'running' || data.status === 'building')) {
        return 2500 // 2.5 seconds for active deployments
      }
      // Poll while waiting for screenshot to be generated
      if (data && data.status === 'completed' && !data.screenshot_location) {
        return 3000 // 3 seconds while waiting for screenshot
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

  // Mutation to disable attack mode
  const queryClient = useQueryClient()
  const disableAttackMode = useMutation({
    ...updateProjectSettingsMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: getProjectBySlugOptions({
          path: { slug: slug || '' },
        }).queryKey,
      })
      toast.success('Attack mode disabled successfully')
      refetch()
    },
    onError: (error: any) => {
      toast.error(
        error?.message || 'Failed to disable attack mode. Please try again.'
      )
    },
  })

  const handleDisableAttackMode = () => {
    if (!project) return

    disableAttackMode.mutate({
      path: { project_id: project.id! },
      body: { attack_mode: false },
    })
  }

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

  usePageTitle(project?.slug ? `${project.slug}` : '')

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
    <MobileSidebarProvider>
      <div className="flex h-full w-full overflow-hidden">
        <Confetti active={showConfetti} duration={4000} particleCount={100} />
        <ProjectDetailSidebar project={project} />
        <div className="flex flex-1 flex-col overflow-hidden">
          <ProjectDetailHeader
            project={project}
            activeVisitorsCount={activeVisitorsCount}
            repositoryCloneUrl={repository?.clone_url}
            lastDeploymentUrl={lastDeployment?.url}
            isLoadingLastDeployment={isLoadingLastDeployment}
          />
          <div className="flex-1 overflow-y-auto p-4">
            {/* Attack Mode Banner */}
            {(project as any).attack_mode && (
              <Alert className="mb-4 border-primary bg-primary/10">
                <ShieldAlert className="h-4 w-4 text-primary" />
                <AlertDescription className="flex items-center justify-between">
                  <span className="text-foreground">
                    Attack Mode is enabled for this project
                  </span>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-7 px-2 text-primary hover:bg-primary/20"
                    onClick={handleDisableAttackMode}
                    disabled={disableAttackMode.isPending}
                  >
                    {disableAttackMode.isPending ? 'Disabling...' : 'Disable'}
                  </Button>
                </AlertDescription>
              </Alert>
            )}
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
                element={
                  <ProjectSettings project={project} refetch={refetch} />
                }
              />
              <Route
                path="speed"
                element={<ProjectSpeedInsights project={project} />}
              />
              <Route
                path="logs/*"
                element={<RequestLogs project={project} />}
              />
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
              <Route
                path="environments/*"
                element={<EnvironmentsTabsView project={project} />}
              />
            </Routes>
          </div>
        </div>
      </div>
    </MobileSidebarProvider>
  )
}
