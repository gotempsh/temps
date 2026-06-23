import { deleteServiceMutation } from '@/api/client/@tanstack/react-query.gen'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Trash2 } from 'lucide-react'
import { useState } from 'react'

interface DeleteServiceButtonProps {
  serviceId: number
  serviceName: string
  onSuccess?: () => void
}

export function DeleteServiceButton({
  serviceId,
  serviceName,
  onSuccess,
}: DeleteServiceButtonProps) {
  const [isOpen, setIsOpen] = useState(false)
  const [confirmName, setConfirmName] = useState('')
  const queryClient = useQueryClient()

  const isConfirmed = confirmName.trim() === serviceName

  const deleteMutation = useMutation({
    ...deleteServiceMutation(),
    meta: {
      errorTitle: 'Failed to delete service',
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['listServices'] })
      setIsOpen(false)
      onSuccess?.()
    },
  })

  const handleOpenChange = (open: boolean) => {
    setIsOpen(open)
    if (!open) {
      setConfirmName('')
    }
  }

  const handleDelete = (e: React.MouseEvent) => {
    e.stopPropagation()
    if (!isConfirmed) return
    deleteMutation.mutate({
      path: {
        id: serviceId,
      },
    })
  }

  const errorMessage = deleteMutation.error
    ? deleteMutation.error instanceof Error
      ? deleteMutation.error.message
      : 'Failed to delete service. Please try again.'
    : null

  return (
    <AlertDialog open={isOpen} onOpenChange={handleOpenChange}>
      <AlertDialogTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className="h-8 w-8 text-muted-foreground hover:text-destructive"
          onClick={(e) => {
            e.stopPropagation()
            setIsOpen(true)
          }}
        >
          <Trash2 className="h-4 w-4" />
        </Button>
      </AlertDialogTrigger>
      <AlertDialogContent onClick={(e) => e.stopPropagation()}>
        <AlertDialogHeader>
          <AlertDialogTitle>Delete storage service?</AlertDialogTitle>
          <AlertDialogDescription>
            This action cannot be undone and all data associated with this
            service will be permanently removed.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <div className="space-y-2">
          <Label htmlFor="confirm-service-name">
            Type{' '}
            <span className="font-mono font-semibold text-foreground">
              {serviceName}
            </span>{' '}
            to confirm
          </Label>
          <Input
            id="confirm-service-name"
            value={confirmName}
            onChange={(e) => setConfirmName(e.target.value)}
            onClick={(e) => e.stopPropagation()}
            placeholder={serviceName}
            autoComplete="off"
            autoFocus
          />
        </div>
        {errorMessage && (
          <div className="rounded-md bg-destructive/10 p-3 text-sm text-destructive">
            {errorMessage}
          </div>
        )}
        <AlertDialogFooter>
          <AlertDialogCancel onClick={(e) => e.stopPropagation()}>
            Cancel
          </AlertDialogCancel>
          <AlertDialogAction
            onClick={handleDelete}
            className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            disabled={deleteMutation.isPending || !isConfirmed}
          >
            {deleteMutation.isPending ? 'Deleting...' : 'Delete'}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
