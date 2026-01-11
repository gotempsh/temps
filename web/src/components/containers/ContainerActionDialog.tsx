import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import {
  startContainerMutation,
  stopContainerMutation,
  restartContainerMutation,
  listContainersOptions,
  getContainerDetailOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'

interface ContainerActionDialogProps {
  projectId: string
  environmentId: string
  action: 'start' | 'stop' | 'restart' | null
  containerId: string | null
  onClose: () => void
  onSuccess?: () => void
}

export function ContainerActionDialog({
  projectId,
  environmentId,
  action,
  containerId,
  onClose,
  onSuccess,
}: ContainerActionDialogProps) {
  const queryClient = useQueryClient()

  const mutation = useMutation({
    mutationFn: async ({
      containerId,
      action,
    }: {
      containerId: string
      action: 'start' | 'stop' | 'restart'
    }) => {
      const baseParams = {
        path: {
          project_id: parseInt(projectId),
          environment_id: parseInt(environmentId),
          container_id: containerId,
        },
      }

      if (action === 'start') {
        const options = startContainerMutation()
        if (options.mutationFn) {
          return await options.mutationFn(baseParams)
        }
      } else if (action === 'stop') {
        const options = stopContainerMutation()
        if (options.mutationFn) {
          return await options.mutationFn(baseParams)
        }
      } else if (action === 'restart') {
        const options = restartContainerMutation()
        if (options.mutationFn) {
          return await options.mutationFn(baseParams)
        }
      }
      throw new Error(`Invalid action: ${action}`)
    },
    onSuccess: (_, { action, containerId }) => {
      // Invalidate the containers list
      queryClient.invalidateQueries({
        queryKey: listContainersOptions({
          path: {
            project_id: parseInt(projectId),
            environment_id: parseInt(environmentId),
          },
        }).queryKey,
      })

      // Invalidate the specific container detail
      queryClient.invalidateQueries({
        queryKey: getContainerDetailOptions({
          path: {
            project_id: parseInt(projectId),
            environment_id: parseInt(environmentId),
            container_id: containerId,
          },
        }).queryKey,
      })

      const actionLabel = action.charAt(0).toUpperCase() + action.slice(1)
      toast.success(`Container ${actionLabel.toLowerCase()}ed successfully`)
      onSuccess?.()
    },
    onError: (error: any, { action }) => {
      toast.error(
        `Failed to ${action} container: ${error?.message || 'Unknown error'}`
      )
    },
  })

  const actionLabels = {
    start: 'Start',
    stop: 'Stop',
    restart: 'Restart',
  }

  const actionDescriptions = {
    start: 'This will start the container.',
    stop: 'This will stop the container. Any unsaved data may be lost.',
    restart:
      'This will restart the container. There may be a brief interruption in service.',
  }

  const handleConfirm = async () => {
    if (!action || !containerId) return

    await mutation.mutateAsync({
      containerId,
      action,
    })
    onClose()
  }

  return (
    <AlertDialog open={!!action} onOpenChange={onClose}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>
            {action ? actionLabels[action] : ''} Container?
          </AlertDialogTitle>
          <AlertDialogDescription>
            {action ? actionDescriptions[action] : ''}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <div className="bg-muted p-3 rounded-md text-sm">
          <p className="text-muted-foreground">This action cannot be undone.</p>
        </div>
        <div className="flex justify-end gap-3">
          <AlertDialogCancel disabled={mutation.isPending}>
            Cancel
          </AlertDialogCancel>
          <AlertDialogAction
            onClick={handleConfirm}
            disabled={mutation.isPending}
            className={
              action === 'stop' || action === 'restart'
                ? 'bg-destructive hover:bg-destructive/90'
                : ''
            }
          >
            {mutation.isPending ? 'Processing...' : 'Confirm'}
          </AlertDialogAction>
        </div>
      </AlertDialogContent>
    </AlertDialog>
  )
}
