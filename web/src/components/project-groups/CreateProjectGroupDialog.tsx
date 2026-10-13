// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useNavigate } from 'react-router'
import { useMutation } from '@tanstack/react-query'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/textarea'
import { createProjectGroupMutation } from '@/api/client/@tanstack/react-query.gen'
import { getErrorMessage } from '@/utils/errorHandling'
import { groupHref } from '@/lib/terminology'
import { ServicePicker, type PickedService } from './ServicePicker'
import { useOpenSession } from './useOpenSession'
import { useInvalidateGroups } from './useProjectGroups'

interface CreateProjectGroupDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** Services to pre-select, e.g. when grouping from a service page. */
  initialServices?: PickedService[]
}

const NO_SERVICES: PickedService[] = []

/**
 * Creates a project (group) and places the chosen services in it, then
 * opens its overview so the user sees what grouping gave them.
 */
export function CreateProjectGroupDialog({
  open,
  onOpenChange,
  initialServices = NO_SERVICES,
}: CreateProjectGroupDialogProps) {
  // Each opening remounts the form, so it starts empty (or pre-selected).
  const session = useOpenSession(open)
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <CreateProjectGroupForm
          key={session}
          open={open}
          onOpenChange={onOpenChange}
          initialServices={initialServices}
        />
      </DialogContent>
    </Dialog>
  )
}

function CreateProjectGroupForm({
  open,
  onOpenChange,
  initialServices,
}: CreateProjectGroupDialogProps & { initialServices: PickedService[] }) {
  const navigate = useNavigate()
  const invalidate = useInvalidateGroups()
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [selected, setSelected] = useState<PickedService[]>(initialServices)
  const [error, setError] = useState<string | null>(null)

  const create = useMutation({ ...createProjectGroupMutation() })

  const submit = async (e: { preventDefault: () => void }) => {
    e.preventDefault()
    setError(null)
    if (!name.trim()) {
      setError('Give the project a name.')
      return
    }
    try {
      const group = await create.mutateAsync({
        body: {
          name: name.trim(),
          description: description.trim() || null,
          project_ids: selected.map((s) => s.id),
        },
      })
      await invalidate()
      toast.success(`Project “${group.name}” created`)
      onOpenChange(false)
      navigate(groupHref(group.slug))
    } catch (err) {
      setError(getErrorMessage(err, 'The project could not be created.'))
    }
  }

  return (
    <form onSubmit={submit} className="space-y-4">
      <DialogHeader>
        <DialogTitle>New project</DialogTitle>
        <DialogDescription>
          Group the services that ship together — say a frontend, an API and a
          worker — to see them in one overview and share environment variables
          between them. Services keep deploying independently.
        </DialogDescription>
      </DialogHeader>

      <div className="space-y-2">
        <Label htmlFor="group-name">Name</Label>
        <Input
          id="group-name"
          placeholder="e.g. Acme Shop"
          value={name}
          onChange={(e) => setName(e.target.value)}
          autoFocus
        />
      </div>

      <div className="space-y-2">
        <Label htmlFor="group-description">
          Description <span className="text-muted-foreground">(optional)</span>
        </Label>
        <Textarea
          id="group-description"
          rows={2}
          placeholder="What these services do together"
          value={description}
          onChange={(e) => setDescription(e.target.value)}
        />
      </div>

      <div className="space-y-2">
        <Label>Services</Label>
        <ServicePicker
          enabled={open}
          selected={selected}
          onChange={setSelected}
          emptyMessage="No services yet. You can create the project now and add services later."
        />
      </div>

      {error && (
        <p role="alert" className="text-sm font-medium text-destructive">
          {error}
        </p>
      )}

      <DialogFooter>
        <Button
          type="button"
          variant="outline"
          onClick={() => onOpenChange(false)}
        >
          Cancel
        </Button>
        <Button type="submit" disabled={create.isPending}>
          {create.isPending
            ? 'Creating…'
            : selected.length > 0
              ? `Create with ${selected.length} ${selected.length === 1 ? 'service' : 'services'}`
              : 'Create project'}
        </Button>
      </DialogFooter>
    </form>
  )
}
