import { ProjectResponse } from '@/api/client'
import {
  deleteEnvironmentMutation,
  getDeploymentOptions,
  getEnvironmentDomainsOptions,
  getEnvironmentOptions,
  getEnvironmentVariablesOptions,
  getEnvironmentVariableValueOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ExternalLink,
  Eye,
  EyeOff,
  RefreshCw,
  Trash2,
} from 'lucide-react'
import { useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { toast } from 'sonner'
import { EnvironmentConfigurationCard } from './EnvironmentConfigurationCard'

interface EnvironmentDetailProps {
  project: ProjectResponse
  environmentId?: number // Optional: if not provided, will use useParams
  initialEnvironment?: any // Optional: initial environment data to use as default
  onDelete?: () => void // Optional: callback after successful deletion
}

function EnvironmentDetailSkeleton() {
  return (
    <div className="space-y-6">
      <div className="flex items-center gap-4">
        <Skeleton className="h-9 w-32" />
      </div>

      <Card>
        <CardHeader>
          <Skeleton className="h-8 w-48" />
          <Skeleton className="h-5 w-96" />
        </CardHeader>
        <CardContent>
          <div className="space-y-6">
            <div>
              <Skeleton className="h-5 w-24 mb-4" />
              <div className="space-y-2">
                {[1, 2].map((i) => (
                  <Skeleton key={i} className="h-10 w-full" />
                ))}
              </div>
            </div>

            <div>
              <Skeleton className="h-5 w-40 mb-4" />
              <div className="space-y-2">
                {[1, 2, 3].map((i) => (
                  <Skeleton key={i} className="h-10 w-full" />
                ))}
              </div>
            </div>
          </div>
        </CardContent>
      </Card>
    </div>
  )
}

interface EnvironmentVariableRowProps {
  variable: any
  project: ProjectResponse
}

function EnvironmentVariableRow({
  variable,
  project,
}: EnvironmentVariableRowProps) {
  const [isVisible, setIsVisible] = useState(false)

  const { data, refetch } = useQuery({
    ...getEnvironmentVariableValueOptions({
      path: {
        project_id: project.id,
        key: variable.key,
      },
    }),
    enabled: isVisible,
  })

  const toggleVisibility = async () => {
    setIsVisible(!isVisible)
    if (!isVisible) {
      refetch()
    }
  }

  return (
    <div className="flex items-center justify-between gap-2 p-2 border rounded-md overflow-hidden">
      <span className="font-mono text-sm truncate min-w-0">{variable.key}</span>
      <div className="flex items-center gap-2 shrink-0">
        {isVisible ? (
          <span className="font-mono text-sm truncate max-w-[120px] sm:max-w-none">{data?.value}</span>
        ) : (
          <span className="font-mono text-sm">••••••••</span>
        )}
        <Button variant="ghost" size="sm" onClick={toggleVisibility}>
          {isVisible ? (
            <EyeOff className="h-4 w-4" />
          ) : (
            <Eye className="h-4 w-4" />
          )}
        </Button>
      </div>
    </div>
  )
}

function CurrentDeployment({
  project,
  deploymentId,
}: {
  project: ProjectResponse
  deploymentId: number
}) {
  const { data: deployment, isLoading } = useQuery({
    ...getDeploymentOptions({
      path: {
        project_id: project.id,
        deployment_id: deploymentId,
      },
    }),
    enabled: !!deploymentId,
  })

  if (isLoading) {
    return (
      <div className="rounded-lg border p-4">
        <div className="flex items-center justify-between">
          <Skeleton className="h-5 w-[200px]" />
          <Skeleton className="h-6 w-[100px]" />
        </div>
      </div>
    )
  }

  if (!deployment) return null

  return (
    <div className="rounded-lg border p-3 sm:p-4">
      <div className="flex flex-col sm:flex-row sm:items-center justify-between gap-2">
        <div className="flex items-center gap-2 min-w-0">
          <Badge
            variant={
              deployment.status === 'success'
                ? 'success'
                : deployment.status === 'failed'
                  ? 'destructive'
                  : 'secondary'
            }
            className="shrink-0"
          >
            {deployment.status}
          </Badge>
          <span className="text-sm text-muted-foreground">Deployed </span>
          <TimeAgo
            date={deployment.created_at}
            className="text-sm text-muted-foreground"
          />
        </div>
        <Button variant="outline" size="sm" asChild className="w-full sm:w-auto">
          <Link to={`/projects/${project.slug}/deployments/${deployment.id}`}>
            View Deployment
          </Link>
        </Button>
      </div>
    </div>
  )
}

function PurgeAssetCacheCard({
  projectId,
  environmentId,
}: {
  projectId: number
  environmentId: number
}) {
  const [isPurging, setIsPurging] = useState(false)
  const [showConfirm, setShowConfirm] = useState(false)

  const handlePurge = async () => {
    setIsPurging(true)
    try {
      const response = await fetch(
        `/api/projects/${projectId}/environments/${environmentId}/asset-cache`,
        { method: 'DELETE' }
      )
      const data = await response.json()
      const deleted = data?.deleted ?? 0
      toast.success(`Purged ${deleted} cached asset${deleted !== 1 ? 's' : ''}`)
    } catch {
      toast.error('Failed to purge asset cache')
    } finally {
      setIsPurging(false)
      setShowConfirm(false)
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-sm font-medium">Asset Cache</CardTitle>
        <CardDescription>
          Static assets (JS chunks, CSS, fonts) are cached for stale-chunk fallback.
          Purge if you need to force-clear cached assets for this environment.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <AlertDialog open={showConfirm} onOpenChange={setShowConfirm}>
          <AlertDialogTrigger asChild>
            <Button variant="outline" size="sm">
              <RefreshCw className="h-4 w-4 mr-2" />
              Purge Asset Cache
            </Button>
          </AlertDialogTrigger>
          <AlertDialogContent>
            <AlertDialogTitle>Purge Asset Cache</AlertDialogTitle>
            <AlertDialogDescription>
              This will delete all cached static assets for this environment.
              In-flight users with old HTML may see broken pages until they refresh.
              Orphaned blobs are cleaned up automatically overnight.
            </AlertDialogDescription>
            <div className="flex justify-end gap-3 mt-4">
              <AlertDialogCancel>Cancel</AlertDialogCancel>
              <AlertDialogAction
                onClick={handlePurge}
                disabled={isPurging}
              >
                {isPurging ? 'Purging...' : 'Purge Cache'}
              </AlertDialogAction>
            </div>
          </AlertDialogContent>
        </AlertDialog>
      </CardContent>
    </Card>
  )
}

export function EnvironmentDetail({
  project,
  environmentId: propEnvironmentId,
  initialEnvironment,
  onDelete,
}: EnvironmentDetailProps) {
  const { environmentId: paramEnvironmentId } = useParams<{
    environmentId: string
  }>()
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false)
  const queryClient = useQueryClient()

  // Use prop if provided, otherwise use URL param
  const environmentId = propEnvironmentId ?? Number(paramEnvironmentId)

  // Use the passed initialEnvironment if available, otherwise fetch
  const {
    data: environment = initialEnvironment,
    isLoading: isLoadingEnvironment,
    error: environmentError,
  } = useQuery({
    ...getEnvironmentOptions({
      path: {
        project_id: project.id,
        env_id: Number(environmentId!),
      },
    }),
    initialData: initialEnvironment,
    staleTime: Infinity, // Keep initial data fresh indefinitely
    gcTime: 1000 * 60 * 10, // 10 minutes - keep in cache
    enabled: !initialEnvironment, // Only fetch if we don't have initial data
  })

  const {
    data: variables,
    isLoading: isLoadingVariables,
    error: variablesError,
  } = useQuery({
    ...getEnvironmentVariablesOptions({
      path: {
        project_id: project.id,
      },
    }),
    select: (data) =>
      data.filter((v) => v.environments.some((e) => e.id === environmentId)),
  })

  const {
    data: domains,
    isLoading: isLoadingDomains,
    error: domainsError,
  } = useQuery({
    ...getEnvironmentDomainsOptions({
      path: {
        project_id: project.id,
        env_id: Number(environmentId!),
      },
    }),
  })

  const removeEnvironmentMutation = useMutation({
    ...deleteEnvironmentMutation(),
    onSuccess: () => {
      toast.success('Environment deleted successfully')
      setShowDeleteConfirm(false)
      queryClient.invalidateQueries({ queryKey: ['environments'] })

      // Call the onDelete callback if provided, otherwise fallback to history.back()
      if (onDelete) {
        onDelete()
      } else {
        window.history.back()
      }
    },
    onError: (error: any) => {
      toast.error(error?.message || 'Failed to delete environment')
    },
  })

  if (isLoadingEnvironment || isLoadingVariables || isLoadingDomains) {
    return <EnvironmentDetailSkeleton />
  }

  if (environmentError) {
    return (
      <ErrorAlert
        title="Error loading environment"
        description={environmentError.message}
      />
    )
  }

  if (variablesError) {
    return (
      <ErrorAlert
        title="Error loading environment variables"
        description={variablesError.message}
      />
    )
  }

  if (domainsError) {
    return (
      <ErrorAlert
        title="Error loading domains"
        description={domainsError.message}
      />
    )
  }

  if (!environment) return null

  // Check if this is a production environment
  const isProduction = environment.slug === 'production'

  return (
    <div className="space-y-6">
      {environment.current_deployment_id && (
        <CurrentDeployment
          project={project}
          deploymentId={environment.current_deployment_id}
        />
      )}

      <Card>
        <CardHeader>
          <div className="flex flex-col sm:flex-row sm:items-start sm:justify-between gap-2">
            <div>
              <CardTitle>Domains</CardTitle>
              <CardDescription>
                Custom domains attached to this environment
              </CardDescription>
            </div>
            <Button variant="outline" size="sm" asChild>
              <Link to={`/projects/${project.slug}/settings/domains`}>
                Manage in Domains
                <ExternalLink className="h-4 w-4 ml-2" />
              </Link>
            </Button>
          </div>
        </CardHeader>
        <CardContent>
          {domains?.length ? (
            <div className="space-y-2">
              {domains.map((domain) => (
                <div
                  key={domain.id}
                  className="flex items-center justify-between rounded-lg border p-3 gap-2"
                >
                  <span className="font-mono text-sm truncate">
                    {domain.domain}
                  </span>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-8 w-8 shrink-0"
                    onClick={() =>
                      window.open(domain.url ?? `https://${domain.domain}`, '_blank')
                    }
                    aria-label={`Visit ${domain.domain}`}
                  >
                    <ExternalLink className="h-4 w-4" />
                  </Button>
                </div>
              ))}
            </div>
          ) : (
            <p className="text-sm text-muted-foreground">
              No domains attached. Add one from the project Domains tab.
            </p>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Environment Variables</CardTitle>
          <CardDescription>
            Manage environment-specific variables
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="space-y-4">
            {variables?.length ? (
              <div className="space-y-2">
                {variables.map((variable) => (
                  <EnvironmentVariableRow
                    key={variable.id}
                    variable={variable}
                    project={project}
                  />
                ))}
              </div>
            ) : (
              <p className="text-sm text-muted-foreground">
                No environment variables configured
              </p>
            )}
          </div>
        </CardContent>
      </Card>

      <EnvironmentConfigurationCard
        project={project}
        environment={environment}
        onUpdate={() => {
          queryClient.invalidateQueries({ queryKey: ['environment'] })
        }}
      />

      <PurgeAssetCacheCard projectId={project.id} environmentId={environmentId} />

      <Card className="border-destructive/50 bg-destructive/5">
        <CardHeader>
          <CardTitle className="text-destructive">Danger Zone</CardTitle>
          <CardDescription>
            Irreversible and destructive actions
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="space-y-4">
            <p className="text-sm text-muted-foreground">
              Deleting this environment will remove all configurations,
              deployments, and data associated with it. This action cannot be
              undone.
            </p>
            {isProduction && (
              <p className="text-sm text-muted-foreground bg-muted p-3 rounded-md border">
                ℹ️ The production environment cannot be deleted to prevent
                accidental data loss.
              </p>
            )}
            <AlertDialog
              open={showDeleteConfirm}
              onOpenChange={setShowDeleteConfirm}
            >
              <AlertDialogTrigger asChild>
                <Button variant="destructive" disabled={isProduction} className="w-full sm:w-auto">
                  <Trash2 className="h-4 w-4 mr-2" />
                  Delete Environment
                </Button>
              </AlertDialogTrigger>
              <AlertDialogContent>
                <AlertDialogTitle>Delete Environment</AlertDialogTitle>
                <AlertDialogDescription>
                  Are you sure you want to delete the &quot;{environment.name}
                  &quot; environment? This action cannot be undone.
                </AlertDialogDescription>
                <div className="flex justify-end gap-3 mt-6">
                  <AlertDialogCancel>Cancel</AlertDialogCancel>
                  <AlertDialogAction
                    onClick={async () => {
                      await removeEnvironmentMutation.mutateAsync({
                        path: {
                          project_id: project.id || 0,
                          env_id: Number(environmentId),
                        },
                      })
                    }}
                    className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
                    disabled={removeEnvironmentMutation.isPending}
                  >
                    {removeEnvironmentMutation.isPending
                      ? 'Deleting...'
                      : 'Delete Environment'}
                  </AlertDialogAction>
                </div>
              </AlertDialogContent>
            </AlertDialog>
          </div>
        </CardContent>
      </Card>
    </div>
  )
}
