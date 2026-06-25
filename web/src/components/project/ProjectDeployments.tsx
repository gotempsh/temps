import { EnvironmentResponse, ProjectResponse } from '@/api/client'
import {
  cancelDeploymentMutation,
  deployFromImageMutation,
  getEnvironmentsOptions,
  getProjectDeploymentsOptions,
  promoteDeploymentMutation,
  rollbackToDeploymentMutation,
  triggerProjectPipelineMutation,
} from '@/api/client/@tanstack/react-query.gen'
import DeploymentCompactRow from '@/components/deployment/DeploymentCompactRow'
import { RedeploymentModal } from '@/components/deployments/RedeploymentModal'
import { Card } from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Skeleton } from '@/components/ui/skeleton'
import { KeyboardShortcut } from '@/components/ui/keyboard-shortcut'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  getErrorMessage,
  getExpiredTokenMessage,
  isExpiredTokenError,
} from '@/utils/errorHandling'
import { useMutation, useQuery } from '@tanstack/react-query'
import { useState, useCallback, useEffect, useRef } from 'react'
import { Link, useSearchParams } from 'react-router-dom'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/label'
import { ArrowUpRight, ChevronLeft, ChevronRight, Loader2, PlusIcon, RefreshCw } from 'lucide-react'
import { EmptyPlaceholder } from '@/components/ui/empty-placeholder'

const ITEMS_PER_PAGE = 10

export function ProjectDeployments({ project }: { project: ProjectResponse }) {
  const [isRedeployModalOpen, setIsRedeployModalOpen] = useState(false)
  const [selectedDeployment, setSelectedDeployment] = useState<number | null>(
    null
  )
  const [promoteDeploymentId, setPromoteDeploymentId] = useState<number | null>(null)
  const [promoteTargetEnv, setPromoteTargetEnv] = useState<string>('')
  const [searchParams, setSearchParams] = useSearchParams()
  const refreshIntervalRef = useRef<ReturnType<typeof setInterval> | null>(null)
  const initialDeploymentCountRef = useRef<number | null>(null)
  const [currentPage, setCurrentPage] = useState(1)

  // Handle opening new deployment modal
  const handleOpenNewDeployment = useCallback(() => {
    setSelectedDeployment(null)
    setIsRedeployModalOpen(true)
  }, [])
  const {
    data: deploymentsData,
    isLoading,
    isFetching,
    error,
    refetch,
  } = useQuery({
    ...getProjectDeploymentsOptions({
      path: { id: project.id },
      query: {
        page: currentPage,
        per_page: ITEMS_PER_PAGE,
      },
    }),
    retry: false,
    refetchOnWindowFocus: true,
  })

  const totalPages = deploymentsData
    ? Math.ceil(deploymentsData.total / ITEMS_PER_PAGE)
    : 1

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

  // Redeploy path for docker_image projects: re-pull the prebuilt image
  // instead of running the git pipeline (which has no commit/repo to build).
  const redeployImage = useMutation({
    ...deployFromImageMutation(),
    meta: {
      errorTitle: 'Failed to redeploy image',
    },
    onSuccess: () => {
      toast.success('Deployment triggered successfully')
      setIsRedeployModalOpen(false)
      refetch()
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
    onError: (error: any) => {
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

  const rollbackDeployment = useMutation({
    ...rollbackToDeploymentMutation(),
    meta: {
      errorTitle: 'Failed to rollback deployment',
    },
    onSuccess: () => {
      toast.success('Deployment rollback initiated successfully')
      refetch()
    },
    onError: (error: any) => {
      // Check if it's an expired token error
      if (isExpiredTokenError(error)) {
        const message = getExpiredTokenMessage(error)
        toast.error(message)
      } else if (error.detail) {
        toast.error(error.detail)
      } else {
        const errorMessage = getErrorMessage(
          error,
          'Failed to rollback deployment'
        )
        toast.error(errorMessage)
      }
    },
  })

  // Resolve the prebuilt image ref for a docker_image project: prefer the
  // selected deployment's image, else the most recent deployment that has one.
  const resolveImageRef = useCallback((): string | undefined => {
    const deployments = deploymentsData?.deployments ?? []
    if (selectedDeployment != null) {
      const sel = deployments.find((d) => d.id === selectedDeployment)
      if (sel?.metadata?.externalImageRef) return sel.metadata.externalImageRef
    }
    return (
      deployments.find((d) => d.metadata?.externalImageRef)?.metadata
        ?.externalImageRef ?? undefined
    )
  }, [deploymentsData?.deployments, selectedDeployment])

  const imageRef = resolveImageRef()

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
    // docker_image projects re-pull the prebuilt image; git projects run the pipeline.
    if (project.source_type === 'docker_image') {
      const ref = resolveImageRef()
      if (!ref) {
        toast.error('No image reference found for this project')
        return
      }
      await redeployImage.mutateAsync({
        path: { project_id: project.id, environment_id: environmentId },
        body: { image_ref: ref },
      })
      return
    }

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

  const handleRollbackDeployment = async (deploymentId: number) => {
    toast.promise(
      rollbackDeployment.mutateAsync({
        path: {
          project_id: project.id,
          deployment_id: deploymentId,
        },
      }),
      {
        loading: 'Rolling back deployment...',
        success: 'Deployment rollback initiated successfully',
        error: 'Failed to rollback deployment',
      }
    )
  }

  // --- Promote deployment ---
  const environmentsQuery = useQuery({
    ...getEnvironmentsOptions({
      path: { project_id: project.id },
    }),
  })

  const promoteDeploymentMut = useMutation({
    ...promoteDeploymentMutation(),
    meta: {
      errorTitle: 'Failed to promote deployment',
    },
    onSuccess: () => {
      toast.success('Deployment promoted successfully')
      setPromoteDeploymentId(null)
      setPromoteTargetEnv('')
      refetch()
    },
    onError: (error: any) => {
      if (isExpiredTokenError(error)) {
        toast.error(getExpiredTokenMessage(error))
      } else if (error.detail) {
        toast.error(error.detail)
      } else {
        toast.error(getErrorMessage(error, 'Failed to promote deployment'))
      }
    },
  })

  const handlePromoteDeployment = async () => {
    if (!promoteDeploymentId || !promoteTargetEnv) return
    toast.promise(
      promoteDeploymentMut.mutateAsync({
        path: {
          project_id: project.id,
          deployment_id: promoteDeploymentId,
        },
        body: {
          target_environment_id: parseInt(promoteTargetEnv),
        },
      }),
      {
        loading: 'Promoting deployment...',
        success: 'Deployment promoted successfully',
        error: 'Failed to promote deployment',
      }
    )
  }

  // Get environments that are different from the deployment's environment
  const getPromoteTargetEnvironments = (deploymentId: number) => {
    const deployment = deploymentsData?.deployments.find(
      (d) => d.id === deploymentId
    )
    if (!deployment || !environmentsQuery.data) return []
    return (environmentsQuery.data as EnvironmentResponse[]).filter(
      (env) => env.id !== deployment.environment_id
    )
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
          mode={selectedDeployment ? 'redeploy' : 'new'}
          defaultBranch={
            deploymentsData?.deployments.find(
              (d) => d.id === selectedDeployment
            )?.branch ?? project.main_branch
          }
          defaultCommit={
            deploymentsData?.deployments.find(
              (d) => d.id === selectedDeployment
            )?.commit_hash ?? ''
          }
          defaultTag={
            deploymentsData?.deployments.find(
              (d) => d.id === selectedDeployment
            )?.tag ?? ''
          }
          defaultType={(() => {
            const deployment = deploymentsData?.deployments.find(
              (d) => d.id === selectedDeployment,
            )
            if (!deployment) return 'branch' // Default to branch for new deployments
            if (deployment?.tag) return 'tag'
            if (deployment?.branch) return 'branch'
            return 'commit'
          })()}
          defaultEnvironment={
            deploymentsData?.deployments.find(
              (d) => d.id === selectedDeployment
            )?.environment_id ?? undefined
          }
          isLoading={createDeployment.isPending || redeployImage.isPending}
          imageRef={imageRef}
        />
      </>
    )
  }

  return (
    <>
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between mb-4">
        <div className="flex items-center gap-2">
          <h2 className="text-lg font-semibold">Deployments</h2>
          <Button
            variant="ghost"
            size="icon"
            className="h-7 w-7"
            onClick={() => refetch()}
            disabled={isFetching}
          >
            {isFetching ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <RefreshCw className="h-3.5 w-3.5" />}
          </Button>
        </div>
        <Button onClick={handleOpenNewDeployment} className="w-full sm:w-auto">
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
              <DeploymentCompactRow
                deployment={deployment}
                onRedeploy={() => {
                  setSelectedDeployment(deployment.id)
                  setIsRedeployModalOpen(true)
                }}
                onCancel={() => handleCancelDeployment(deployment.id)}
                onRollback={() => handleRollbackDeployment(deployment.id)}
                onPromote={() => {
                  setPromoteDeploymentId(deployment.id)
                  setPromoteTargetEnv('')
                }}
              />
            </Link>
          ))}
        </ul>
      </Card>

      {/* Pagination */}
      {totalPages > 1 && (
        <div className="flex items-center justify-between mt-4">
          <p className="text-sm text-muted-foreground">
            <span className="hidden sm:inline">
              Showing {(currentPage - 1) * ITEMS_PER_PAGE + 1}–{Math.min(currentPage * ITEMS_PER_PAGE, deploymentsData.total)} of {deploymentsData.total}
            </span>
            <span className="sm:hidden">
              {currentPage} / {totalPages}
            </span>
          </p>
          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => setCurrentPage((p) => Math.max(1, p - 1))}
              disabled={currentPage <= 1 || isFetching}
            >
              <ChevronLeft className="h-4 w-4" />
              <span className="hidden sm:inline ml-1">Previous</span>
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setCurrentPage((p) => Math.min(totalPages, p + 1))}
              disabled={currentPage >= totalPages || isFetching}
            >
              <span className="hidden sm:inline mr-1">Next</span>
              <ChevronRight className="h-4 w-4" />
            </Button>
          </div>
        </div>
      )}

      <RedeploymentModal
        project={project}
        isOpen={isRedeployModalOpen}
        onClose={() => {
          setIsRedeployModalOpen(false)
          setSelectedDeployment(null)
        }}
        onConfirm={handleRedeploy}
        mode={selectedDeployment ? 'redeploy' : 'new'}
        defaultBranch={
          deploymentsData?.deployments.find((d) => d.id === selectedDeployment)
            ?.branch ?? project.main_branch
        }
        defaultCommit={
          deploymentsData?.deployments.find((d) => d.id === selectedDeployment)
            ?.commit_hash ?? ''
        }
        defaultTag={
          deploymentsData?.deployments.find((d) => d.id === selectedDeployment)
            ?.tag ?? ''
        }
        defaultType={(() => {
          const deployment = deploymentsData?.deployments.find(
            (d) => d.id === selectedDeployment,
          )
          if (!deployment) return 'branch' // Default to branch for new deployments
          if (deployment?.tag) return 'tag'
          if (deployment?.branch) return 'branch'
          return 'commit'
        })()}
        defaultEnvironment={
          deploymentsData?.deployments.find((d) => d.id === selectedDeployment)
            ?.environment_id ?? undefined
        }
        isLoading={createDeployment.isPending || redeployImage.isPending}
        imageRef={imageRef}
      />

      {/* Promote deployment dialog */}
      <Dialog
        open={promoteDeploymentId !== null}
        onOpenChange={(open) => {
          if (!open) {
            setPromoteDeploymentId(null)
            setPromoteTargetEnv('')
          }
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <ArrowUpRight className="h-5 w-5" />
              Promote Deployment
            </DialogTitle>
            <DialogDescription>
              Deploy the same image to another environment. This is useful for
              promoting a validated deployment to production.
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4 py-4">
            <div className="space-y-2">
              <Label>Target Environment</Label>
              <Select value={promoteTargetEnv} onValueChange={setPromoteTargetEnv}>
                <SelectTrigger>
                  <SelectValue placeholder="Select environment..." />
                </SelectTrigger>
                <SelectContent>
                  {promoteDeploymentId &&
                    getPromoteTargetEnvironments(promoteDeploymentId).map(
                      (env) => (
                        <SelectItem key={env.id} value={String(env.id)}>
                          {env.name}
                          {env.protected && ' (protected)'}
                        </SelectItem>
                      )
                    )}
                </SelectContent>
              </Select>
            </div>
          </div>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => {
                setPromoteDeploymentId(null)
                setPromoteTargetEnv('')
              }}
            >
              Cancel
            </Button>
            <Button
              onClick={handlePromoteDeployment}
              disabled={!promoteTargetEnv || promoteDeploymentMut.isPending}
            >
              {promoteDeploymentMut.isPending ? 'Promoting...' : 'Promote'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
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
