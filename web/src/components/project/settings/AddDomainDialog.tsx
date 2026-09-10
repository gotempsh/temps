// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import { getEnvironmentsOptions } from '@/api/client/@tanstack/react-query.gen'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { useQuery } from '@tanstack/react-query'
import { DomainForm } from './DomainForm'

interface AddDomainDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  project: ProjectResponse
  onSuccess: () => void
}

export function AddDomainDialog({
  open,
  onOpenChange,
  project,
  onSuccess,
}: AddDomainDialogProps) {
  const { data: environments } = useQuery({
    ...getEnvironmentsOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-[600px]">
        <DialogHeader>
          <DialogTitle>Add Domain</DialogTitle>
        </DialogHeader>
        {environments && (
          <DomainForm
            project_id={project.id}
            environments={environments}
            preset={project.preset}
            onSuccess={onSuccess}
            onCancel={() => onOpenChange(false)}
          />
        )}
      </DialogContent>
    </Dialog>
  )
}
