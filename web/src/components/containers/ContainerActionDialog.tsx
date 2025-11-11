import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { useContainerAction } from '@/hooks/containers'

interface ContainerActionDialogProps {
  projectId: string
  environmentId: string
  action: 'start' | 'stop' | 'restart' | null
  containerId: string | null
  onClose: () => void
}

export function ContainerActionDialog({
  projectId,
  environmentId,
  action,
  containerId,
  onClose,
}: ContainerActionDialogProps) {
  const mutation = useContainerAction(projectId, environmentId)

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
