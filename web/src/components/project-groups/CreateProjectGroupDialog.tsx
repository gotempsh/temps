// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useNavigate } from 'react-router'
import { useMutation } from '@tanstack/react-query'
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import * as z from 'zod/v4'
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
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
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

const createGroupSchema = z.object({
  name: z.string().trim().min(1, 'Give the project a name.'),
  description: z.string(),
})

type CreateGroupValues = z.infer<typeof createGroupSchema>

function CreateProjectGroupForm({
  open,
  onOpenChange,
  initialServices,
}: CreateProjectGroupDialogProps & { initialServices: PickedService[] }) {
  const navigate = useNavigate()
  const invalidate = useInvalidateGroups()
  // The service choice is a selection, not a text field: it stays outside
  // the form values.
  const [selected, setSelected] = useState<PickedService[]>(initialServices)

  const form = useForm<CreateGroupValues>({
    resolver: zodResolver(createGroupSchema),
    defaultValues: { name: '', description: '' },
  })

  const create = useMutation({ ...createProjectGroupMutation() })

  const submit = async (values: CreateGroupValues) => {
    try {
      const group = await create.mutateAsync({
        body: {
          name: values.name,
          description: values.description.trim() || null,
          project_ids: selected.map((s) => s.id),
        },
      })
      await invalidate()
      toast.success(`Project “${group.name}” created`)
      onOpenChange(false)
      navigate(groupHref(group.slug))
    } catch (err) {
      form.setError('root', {
        type: 'server',
        message: getErrorMessage(err, 'The project could not be created.'),
      })
    }
  }

  return (
    <Form {...form}>
      <form
        onSubmit={(event) => void form.handleSubmit(submit)(event)}
        className="space-y-4"
      >
        <DialogHeader>
          <DialogTitle>New project</DialogTitle>
          <DialogDescription>
            Group the services that ship together — say a frontend, an API and a
            worker — to see them in one overview and share environment variables
            between them. Services keep deploying independently.
          </DialogDescription>
        </DialogHeader>

        <FormField
          control={form.control}
          name="name"
          render={({ field }) => (
            <FormItem>
              <FormLabel htmlFor="group-name">Name</FormLabel>
              <FormControl>
                <Input
                  {...field}
                  id="group-name"
                  placeholder="e.g. Acme Shop"
                  autoFocus
                />
              </FormControl>
              <FormMessage />
            </FormItem>
          )}
        />

        <FormField
          control={form.control}
          name="description"
          render={({ field }) => (
            <FormItem>
              <FormLabel htmlFor="group-description">
                Description{' '}
                <span className="text-muted-foreground">(optional)</span>
              </FormLabel>
              <FormControl>
                <Textarea
                  {...field}
                  id="group-description"
                  rows={2}
                  placeholder="What these services do together"
                />
              </FormControl>
              <FormMessage />
            </FormItem>
          )}
        />

        <div className="space-y-2">
          <Label>Services</Label>
          <ServicePicker
            enabled={open}
            selected={selected}
            onChange={setSelected}
            emptyMessage="No services yet. You can create the project now and add services later."
          />
        </div>

        {form.formState.errors.root && (
          <p role="alert" className="text-sm font-medium text-destructive">
            {form.formState.errors.root.message}
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
    </Form>
  )
}
