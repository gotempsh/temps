import { ProjectResponse } from '@/api/client'
import {
  deleteEnvironmentMutation,
  getEnvironmentOptions,
} from '@/api/client/@tanstack/react-query.gen'
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
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { RefreshCw, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { useParams } from 'react-router-dom'
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

  if (isLoadingEnvironment) {
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

  if (!environment) return null

  // Check if this is a production environment
  const isProduction = environment.slug === 'production'

  return (
    <div className="space-y-6">
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
