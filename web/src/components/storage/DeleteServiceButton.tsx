// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { deleteServiceMutation } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
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
  const queryClient = useQueryClient()

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

  return (
    <>
      <Button
        variant="ghost"
        size="icon"
        className="h-8 w-8 text-muted-foreground hover:text-destructive"
        aria-label={`Delete ${serviceName}`}
        onClick={(e) => {
          e.stopPropagation()
          setIsOpen(true)
        }}
      >
        <Trash2 className="h-4 w-4" />
      </Button>
      {/* The dialog is portaled, but React events still bubble to the
          clickable row this button lives in; contain them here. */}
      <span className="contents" onClick={(e) => e.stopPropagation()}>
        <ConfirmDeleteDialog
          open={isOpen}
          onOpenChange={setIsOpen}
          resourceType="storage service"
          resourceName={serviceName}
          consequences="All data stored in this service will be permanently removed, and anything linked to it will lose access."
          requireTypedName
          isPending={deleteMutation.isPending}
          onConfirm={() => deleteMutation.mutate({ path: { id: serviceId } })}
        />
      </span>
    </>
  )
}
