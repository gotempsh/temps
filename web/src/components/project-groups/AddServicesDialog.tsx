// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo, useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import { toast } from 'sonner'
import { Link } from 'react-router'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { addProjectToGroupMutation } from '@/api/client/@tanstack/react-query.gen'
import type { ProjectGroupResponse } from '@/api/client'
import { getErrorMessage } from '@/utils/errorHandling'
import { ServicePicker, type PickedService } from './ServicePicker'
import { newServiceInGroupHref } from './useJoinGroupAfterCreate'
import { useOpenSession } from './useOpenSession'
import { useInvalidateGroups } from './useProjectGroups'

interface AddServicesDialogProps {
  group: ProjectGroupResponse
  open: boolean
  onOpenChange: (open: boolean) => void
}

/** Adds existing services to a project, or points at creating a new one. */
export function AddServicesDialog({
  group,
  open,
  onOpenChange,
}: AddServicesDialogProps) {
  // Each opening remounts the form, so it never shows a previous selection.
  const session = useOpenSession(open)
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <AddServicesForm
          key={session}
          group={group}
          open={open}
          onOpenChange={onOpenChange}
        />
      </DialogContent>
    </Dialog>
  )
}

function AddServicesForm({
  group,
  open,
  onOpenChange,
}: AddServicesDialogProps) {
  const invalidate = useInvalidateGroups()
  const [selected, setSelected] = useState<PickedService[]>([])
  const [error, setError] = useState<string | null>(null)
  const add = useMutation({ ...addProjectToGroupMutation() })

  const members = useMemo(
    () => new Set(group.projects.map((p) => p.id)),
    [group.projects]
  )

  const submit = async () => {
    setError(null)
    const failures: string[] = []
    // Sequential so a failure names exactly which service it was.
    for (const service of selected) {
      try {
        await add.mutateAsync({
          path: { group_id: group.id, project_id: service.id },
        })
      } catch (err) {
        failures.push(
          `${service.name}: ${getErrorMessage(err, 'could not be added')}`
        )
      }
    }
    await invalidate()
    const added = selected.length - failures.length
    if (added > 0) {
      toast.success(
        `Added ${added} ${added === 1 ? 'service' : 'services'} to ${group.name}`
      )
    }
    if (failures.length > 0) {
      setError(failures.join('\n'))
      setSelected([])
      return
    }
    onOpenChange(false)
  }

  return (
    <>
      <DialogHeader>
        <DialogTitle>Add services to {group.name}</DialogTitle>
        <DialogDescription>
          Added services inherit this project&apos;s shared variables on their
          next deployment. Their own variables still win.
        </DialogDescription>
      </DialogHeader>

      <ServicePicker
        enabled={open}
        excludeIds={members}
        selected={selected}
        onChange={setSelected}
        emptyMessage={
          group.projects.length > 0
            ? 'Every service is already in this project.'
            : 'No services yet. Create one below; it is added to this project.'
        }
      />
      <p className="text-sm text-muted-foreground">
        Need a new one?{' '}
        <Link
          to={newServiceInGroupHref(group.id)}
          className="font-medium text-foreground underline"
        >
          Create a service
        </Link>{' '}
        and it is added to {group.name} when it is created.
      </p>

      {error && (
        <p
          role="alert"
          className="whitespace-pre-line text-sm font-medium text-destructive"
        >
          {error}
        </p>
      )}

      <DialogFooter>
        <Button variant="outline" onClick={() => onOpenChange(false)}>
          Cancel
        </Button>
        <Button
          onClick={() => void submit()}
          disabled={selected.length === 0 || add.isPending}
        >
          {add.isPending ? 'Adding…' : addLabel(selected.length)}
        </Button>
      </DialogFooter>
    </>
  )
}

function addLabel(count: number): string {
  if (count === 0) return 'Add services'
  return `Add ${count} ${count === 1 ? 'service' : 'services'}`
}
