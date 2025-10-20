import { ProjectResponse } from '@/api/client'
import {
  cancelDeploymentMutation,
  getProjectDeploymentsOptions,
  triggerProjectPipelineMutation,
} from '@/api/client/@tanstack/react-query.gen'
import DeploymentListItem from '@/components/deployment/DeploymentListItem'
import { RedeploymentModal } from '@/components/deployments/RedeploymentModal'
import { Card } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { KeyboardShortcut } from '@/components/ui/keyboard-shortcut'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import {
  getErrorMessage,
  getExpiredTokenMessage,
  isExpiredTokenError,
} from '@/utils/errorHandling'
import { useMutation, useQuery } from '@tanstack/react-query'
import { useState, useCallback, useEffect, useRef } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router-dom'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { PlusIcon } from 'lucide-react'
import { EmptyPlaceholder } from '@/components/ui/empty-placeholder'

const ITEMS_PER_PAGE = 10

export function ProjectDeployments({ project }: { project: ProjectResponse }) {
  const [isRedeployModalOpen, setIsRedeployModalOpen] = useState(false)
  const [selectedDeployment, setSelectedDeployment] = useState<number | null>(
    null
  )
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()
  const refreshIntervalRef = useRef<NodeJS.Timeout | null>(null)
  const initialDeploymentCountRef = useRef<number | null>(null)

  // Handle opening new deployment modal
  const handleOpenNewDeployment = useCallback(() => {
    setSelectedDeployment(null)
    setIsRedeployModalOpen(true)
  }, [])
  const {
    data: deploymentsData,
    isLoading,
    error,
    refetch,
  } = useQuery({
    ...getProjectDeploymentsOptions({
      path: { id: project?.id! },
      query: {
        page: 1,
        per_page: ITEMS_PER_PAGE,
      },
    }),
    retry: false,
  })

  // Auto-refresh when coming from deployment details
  useEffect(() => {
    const autoRefresh = searchParams.get('autoRefresh')

    if (autoRefresh === 'true' && deploymentsData?.deployments.length) {
      // Store initial deployment count
      if (initialDeploymentCountRef.current === null) {
        initialDeploymentCountRef.current = deploymentsData.deployments.length
      }

      // Check if a new deployment appeared
      const hasNewDeployment =
        deploymentsData.deployments.length > initialDeploymentCountRef.current

      if (hasNewDeployment) {
        // New deployment found, stop refreshing and clear the query param
        if (refreshIntervalRef.current) {
          clearInterval(refreshIntervalRef.current)
          refreshIntervalRef.current = null
        }
        setSearchParams({}, { replace: true })
        initialDeploymentCountRef.current = null
        toast.success('New deployment detected!')
      } else {
        // No new deployment yet, set up refresh interval
        if (!refreshIntervalRef.current) {
          refreshIntervalRef.current = setInterval(() => {
            refetch()
          }, 1000)
        }
      }
    }
  }, [deploymentsData, searchParams, setSearchParams, refetch])

  // Cleanup interval on unmount
  useEffect(() => {
    return () => {
      if (refreshIntervalRef.current) {
        clearInterval(refreshIntervalRef.current)
      }
      initialDeploymentCountRef.current = null
    }
  }, [])

  const createDeployment = useMutation({
    ...triggerProjectPipelineMutation(),
    meta: {
      errorTitle: 'Failed to trigger deployment',
    },
    onSuccess: () => {
      toast.success('Deployment triggered successfully')
      setIsRedeployModalOpen(false)

      // Clear any existing interval
      if (refreshIntervalRef.current) {
        clearInterval(refreshIntervalRef.current)
      }

      // Refresh immediately
      refetch()

      // Set up interval to refresh every 1 second for 5 seconds
      let refreshCount = 0
      refreshIntervalRef.current = setInterval(() => {
        refreshCount++
        refetch()

        if (refreshCount >= 5) {
          if (refreshIntervalRef.current) {
            clearInterval(refreshIntervalRef.current)
            refreshIntervalRef.current = null
          }
        }
      }, 1000)
    },
  })

  const cancelDeployment = useMutation({
    ...cancelDeploymentMutation(),
    meta: {
      errorTitle: 'Failed to cancel deployment',
    },
    onSuccess: () => {
      toast.success('Deployment cancelled successfully')
      refetch()
    },
    onError: (error: unknown) => {
      // Check if it's an expired token error
      if (isExpiredTokenError(error)) {
        const message = getExpiredTokenMessage(error)
        toast.error(message)
      } else {
        const errorMessage = getErrorMessage(
          error,
          'Failed to cancel deployment'
        )
        toast.error(errorMessage)
      }
    },
  })

  const handleRedeploy = async ({
    branch,
    commit,
    tag,
    environmentId,
  }: {
    branch?: string
    commit?: string
    tag?: string
    environmentId: number
  }) => {
    await createDeployment.mutateAsync({
      path: { id: project.id },
      body: {
        branch,
        commit,
        tag,
        environment_id: environmentId,
      },
    })
  }

  const handleCancelDeployment = async (deploymentId: number) => {
    await cancelDeployment.mutateAsync({
      path: {
        project_id: project.id,
        deployment_id: deploymentId,
      },
    })
  }

  if (error) {
    return (
      <ErrorAlert
        title="Failed to load deployments"
        description={
          error instanceof Error
            ? error.message
            : 'An unexpected error occurred'
        }
        retry={() => refetch()}
      />
    )
  }

  if (isLoading) {
    return (
      <Card>
        <div className="divide-y divide-border">
          {Array.from({ length: 3 }).map((_, i) => (
            <div key={i} className="p-4">
              <div className="space-y-3">
                <div className="flex items-center gap-2">
                  <Skeleton className="h-4 w-16" />
                  <Skeleton className="h-5 w-20" />
                  <Skeleton className="h-5 w-20" />
                </div>
                <div className="flex items-center gap-2">
                  <Skeleton className="h-4 w-4" />
                  <Skeleton className="h-4 w-24" />
                  <Skeleton className="h-4 w-4" />
                  <Skeleton className="h-4 w-32" />
                </div>
              </div>
            </div>
          ))}
        </div>
      </Card>
    )
  }

  if (!deploymentsData?.deployments.length) {
    return (
      <>
        <EmptyPlaceholder
          className="border-2 border-dashed"
          icon={PlusIcon}
          title="No deployments"
          description="Get started by creating your first deployment"
          action={
            <Button onClick={handleOpenNewDeployment}>
              <PlusIcon className="h-4 w-4 mr-2" />
              New Deployment
              <KeyboardShortcut
                shortcut="N"
                onTrigger={handleOpenNewDeployment}
              />
            </Button>
          }
        />
        <RedeploymentModal
          project={project}
          isOpen={isRedeployModalOpen}
          onClose={() => {
            setIsRedeployModalOpen(false)
            setSelectedDeployment(null)
          }}
          onConfirm={handleRedeploy}
          defaultBranch={
            deploymentsData?.deployments.find(
              (d) => d.id === selectedDeployment
            )?.branch ?? project.main_branch
          }
          defaultEnvironment={
            deploymentsData?.deployments.find(
              (d) => d.id === selectedDeployment
            )?.environment_id ?? undefined
          }
          isLoading={createDeployment.isPending}
        />
      </>
    )
  }

  return (
    <>
      <div className="flex items-center justify-between mb-4">
        <h2 className="text-lg font-semibold">Deployments</h2>
        <Button onClick={handleOpenNewDeployment}>
          <PlusIcon className="h-4 w-4 mr-2" />
          New Deployment
          <KeyboardShortcut shortcut="N" onTrigger={handleOpenNewDeployment} />
        </Button>
      </div>

      <Card>
        <ul className="divide-y divide-border">
          {deploymentsData.deployments.map((deployment) => (
            <Link
              key={deployment.id}
              to={`/projects/${project.slug}/deployments/${deployment.id}`}
              className="block hover:bg-muted/50 transition-colors"
            >
              <DeploymentListItem
                deployment={deployment}
                onViewDetails={() => {}}
                onRedeploy={() => {
                  setSelectedDeployment(deployment.id)
                  setIsRedeployModalOpen(true)
                }}
                onCancel={() => handleCancelDeployment(deployment.id)}
                onCopyUrl={() => {}}
              />
            </Link>
          ))}
        </ul>
      </Card>

      <RedeploymentModal
        project={project}
        isOpen={isRedeployModalOpen}
        onClose={() => {
          setIsRedeployModalOpen(false)
          setSelectedDeployment(null)
        }}
        onConfirm={handleRedeploy}
        defaultBranch={
          deploymentsData?.deployments.find((d) => d.id === selectedDeployment)
            ?.branch ?? project.main_branch
        }
        defaultEnvironment={
          deploymentsData?.deployments.find((d) => d.id === selectedDeployment)
            ?.environment_id ?? undefined
        }
        isLoading={createDeployment.isPending}
      />
    </>
  )
}

// Create similar components for other sections:
// - ProjectAnalytics
// - ProjectObservability
// - ProjectStorage
// - ProjectDomains
// - ProjectRuntime
// - ProjectSpeed
