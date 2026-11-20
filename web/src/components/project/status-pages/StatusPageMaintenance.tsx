// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  cancelStatusPageMaintenanceMutation,
  createStatusPageMaintenanceMutation,
  listStatusPageMaintenancesOptions,
  updateStatusPageMaintenanceMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { MaintenanceResponse, StatusPageResponse } from '@/api/client'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
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
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { Skeleton } from '@/components/ui/skeleton'
import { Textarea } from '@/components/ui/textarea'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { format } from 'date-fns'
import { CalendarClock, Pencil, Plus, X } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import {
  createMaintenanceRequest,
  isOpen,
  MAINTENANCE_PAGE_SIZE,
  MAINTENANCE_STATE_LABELS,
  maintenanceSchema,
  maintenanceValues,
  MAX_MAINTENANCE_TITLE_CHARS,
  newMaintenanceValues,
  updateMaintenanceRequest,
  type MaintenanceValues,
} from './maintenance'
import { operationQueries } from './status-page-form'

const STATE_VARIANTS: Record<
  MaintenanceResponse['state'],
  'outline' | 'warning-subtle' | 'neutral-subtle'
> = {
  scheduled: 'outline',
  in_progress: 'warning-subtle',
  completed: 'neutral-subtle',
  cancelled: 'neutral-subtle',
}

async function invalidateMaintenance(
  queryClient: ReturnType<typeof useQueryClient>
) {
  await queryClient.invalidateQueries(
    operationQueries('listStatusPageMaintenances')
  )
  await queryClient.invalidateQueries(operationQueries('previewStatusPage'))
}

function windowLabel(item: MaintenanceResponse): string {
  const starts = new Date(item.starts_at)
  const ends = new Date(item.ends_at)
  const sameDay = starts.toDateString() === ends.toDateString()
  return `${format(starts, 'PP p')} – ${format(ends, sameDay ? 'p' : 'PP p')}`
}

/** Maintenance windows of a status page (ADR 108 D2). */
export function StatusPageMaintenance({ page }: { page: StatusPageResponse }) {
  const queryClient = useQueryClient()
  const [pageNumber, setPageNumber] = useState(1)
  const [dialogOpen, setDialogOpen] = useState(false)
  const [editing, setEditing] = useState<MaintenanceResponse | null>(null)
  const [cancelling, setCancelling] = useState<MaintenanceResponse | null>(null)
  const list = useQuery(
    listStatusPageMaintenancesOptions({
      path: { status_page_id: page.id },
      query: { page: pageNumber, page_size: MAINTENANCE_PAGE_SIZE },
    })
  )
  const cancel = useMutation({
    ...cancelStatusPageMaintenanceMutation(),
    meta: { errorTitle: 'Failed to cancel maintenance' },
    onSuccess: async (item) => {
      await invalidateMaintenance(queryClient)
      toast.success(
        item.state === 'cancelled' && new Date(item.starts_at) <= new Date()
          ? 'Maintenance ended. Outage alerts are back on for its components.'
          : 'Maintenance cancelled.'
      )
      setCancelling(null)
    },
    onError: () => setCancelling(null),
  })

  const total = list.data?.total ?? 0
  const noComponents = page.monitors.length === 0
  const openDialog = (item: MaintenanceResponse | null) => {
    setEditing(item)
    setDialogOpen(true)
  }

  return (
    <Card>
      <CardContent className="space-y-4 p-4">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
          <div className="space-y-1">
            <h3 className="flex items-center gap-2 font-semibold">
              <CalendarClock className="size-4" />
              Scheduled maintenance
            </h3>
            <p className="text-sm text-muted-foreground">
              Announce planned work, e.g. “Database upgrade, Sunday
              22:00–23:00”. While it runs its components show as under
              maintenance, outage alerts pause and uptime is not charged.
            </p>
          </div>
          <Button
            onClick={() => openDialog(null)}
            disabled={noComponents}
            className="shrink-0"
          >
            <Plus className="size-4 sm:mr-2" />
            <span className="max-sm:sr-only">Schedule maintenance</span>
          </Button>
        </div>
        {noComponents && (
          <p className="text-sm text-muted-foreground">
            Add components to this page first: maintenance covers its
            components.
          </p>
        )}
        {list.isPending && <Skeleton className="h-32 w-full" />}
        {list.isError && (
          <ErrorAlert
            title="Could not load maintenance windows"
            description={
              list.error instanceof Error
                ? list.error.message
                : String(list.error)
            }
            retry={() => list.refetch()}
          />
        )}
        {list.data && list.data.items.length === 0 && !noComponents && (
          <p className="text-sm text-muted-foreground">Nothing scheduled.</p>
        )}
        {list.data && list.data.items.length > 0 && (
          <ul className="divide-y rounded-md border">
            {list.data.items.map((item) => (
              <MaintenanceRow
                key={item.id}
                item={item}
                page={page}
                onEdit={() => openDialog(item)}
                onCancel={() => setCancelling(item)}
              />
            ))}
          </ul>
        )}
        {total > MAINTENANCE_PAGE_SIZE && (
          <ResponsivePagination
            page={pageNumber}
            pageSize={MAINTENANCE_PAGE_SIZE}
            total={total}
            totalPages={Math.max(
              1,
              Math.ceil(total / MAINTENANCE_PAGE_SIZE),
              pageNumber
            )}
            onPageChange={setPageNumber}
            ariaLabel="Maintenance pagination"
          />
        )}
      </CardContent>
      <MaintenanceDialog
        page={page}
        open={dialogOpen}
        editing={editing}
        onOpenChange={setDialogOpen}
      />
      <AlertDialog
        open={cancelling !== null}
        onOpenChange={(open) => {
          if (!open) setCancelling(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              {cancelling?.state === 'in_progress'
                ? 'End this maintenance now?'
                : 'Cancel this maintenance?'}
            </AlertDialogTitle>
            <AlertDialogDescription>
              {cancelling?.state === 'in_progress'
                ? 'Its components go back to their live status and outage alerts resume. Checks taken so far stay out of uptime.'
                : 'It is removed from the public page. Subscribers are not emailed about the change.'}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Keep it</AlertDialogCancel>
            <AlertDialogAction
              disabled={cancel.isPending}
              onClick={(event) => {
                event.preventDefault()
                if (cancelling) {
                  cancel.mutate({
                    path: {
                      status_page_id: page.id,
                      maintenance_id: cancelling.id,
                    },
                  })
                }
              }}
            >
              {cancel.isPending
                ? 'Saving...'
                : cancelling?.state === 'in_progress'
                  ? 'End now'
                  : 'Cancel maintenance'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </Card>
  )
}

function MaintenanceRow({
  item,
  page,
  onEdit,
  onCancel,
}: {
  item: MaintenanceResponse
  page: StatusPageResponse
  onEdit: () => void
  onCancel: () => void
}) {
  const names = item.monitor_ids.map(
    (id) =>
      page.monitors.find((m) => m.monitor_id === id)?.display_name ??
      `Monitor ${id}`
  )
  return (
    <li className="flex flex-col gap-2 p-3 sm:flex-row sm:items-start sm:justify-between">
      <div className="min-w-0 space-y-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="font-medium [overflow-wrap:anywhere]">
            {item.title}
          </span>
          <Badge variant={STATE_VARIANTS[item.state]}>
            {MAINTENANCE_STATE_LABELS[item.state]}
          </Badge>
        </div>
        <p className="text-sm text-muted-foreground">{windowLabel(item)}</p>
        <p className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
          {names.join(', ')}
        </p>
      </div>
      {isOpen(item) && (
        <div className="flex shrink-0 gap-1">
          <Button
            variant="ghost"
            size="sm"
            onClick={onEdit}
            aria-label={`Edit ${item.title}`}
          >
            <Pencil className="size-4 sm:mr-2" />
            <span className="max-sm:sr-only">Edit</span>
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={onCancel}
            aria-label={`Cancel ${item.title}`}
          >
            <X className="size-4 sm:mr-2" />
            <span className="max-sm:sr-only">
              {item.state === 'in_progress' ? 'End now' : 'Cancel'}
            </span>
          </Button>
        </div>
      )}
    </li>
  )
}

function MaintenanceDialog({
  page,
  open,
  editing,
  onOpenChange,
}: {
  page: StatusPageResponse
  open: boolean
  editing: MaintenanceResponse | null
  onOpenChange: (open: boolean) => void
}) {
  const queryClient = useQueryClient()
  const started = editing?.state === 'in_progress'
  const schema = useMemo(
    () => maintenanceSchema(() => new Date(), started),
    [started]
  )
  const form = useForm<MaintenanceValues>({
    resolver: zodResolver(schema),
    defaultValues: newMaintenanceValues(
      new Date(),
      page.monitors.map((m) => m.monitor_id)
    ),
  })
  useEffect(() => {
    if (!open) return
    form.reset(
      editing
        ? maintenanceValues(editing)
        : newMaintenanceValues(
            new Date(),
            page.monitors.map((m) => m.monitor_id)
          )
    )
  }, [open, editing, page.monitors, form])

  const onSaved = async (message: string) => {
    await invalidateMaintenance(queryClient)
    toast.success(message)
    onOpenChange(false)
  }
  const create = useMutation({
    ...createStatusPageMaintenanceMutation(),
    meta: { errorTitle: 'Failed to schedule maintenance' },
    onSuccess: () =>
      onSaved('Maintenance scheduled. Subscribers and webhooks are told once.'),
  })
  const update = useMutation({
    ...updateStatusPageMaintenanceMutation(),
    meta: { errorTitle: 'Failed to save maintenance' },
    onSuccess: () => onSaved('Maintenance saved.'),
  })
  const pending = create.isPending || update.isPending

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>
            {editing ? 'Edit maintenance' : 'Schedule maintenance'}
          </DialogTitle>
          <DialogDescription>
            {started
              ? 'It has started: only the title, description and end can change.'
              : `Times are in your browser's time zone (${Intl.DateTimeFormat().resolvedOptions().timeZone}).`}
          </DialogDescription>
        </DialogHeader>
        <Form {...form}>
          <form
            className="space-y-4"
            onSubmit={form.handleSubmit((values) => {
              if (editing) {
                update.mutate({
                  path: {
                    status_page_id: page.id,
                    maintenance_id: editing.id,
                  },
                  body: updateMaintenanceRequest(values, started),
                })
              } else {
                create.mutate({
                  path: { status_page_id: page.id },
                  body: createMaintenanceRequest(values),
                })
              }
            })}
          >
            <FormField
              control={form.control}
              name="title"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Title</FormLabel>
                  <FormControl>
                    <Input
                      maxLength={MAX_MAINTENANCE_TITLE_CHARS}
                      placeholder="Database upgrade"
                      {...field}
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
              <FormField
                control={form.control}
                name="startsAt"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Starts</FormLabel>
                    <FormControl>
                      <Input
                        type="datetime-local"
                        disabled={started}
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="endsAt"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Ends</FormLabel>
                    <FormControl>
                      <Input type="datetime-local" {...field} />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>
            <FormField
              control={form.control}
              name="monitorIds"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Affected components</FormLabel>
                  <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
                    {page.monitors.map((monitor) => {
                      const checked = field.value.includes(monitor.monitor_id)
                      return (
                        <label
                          key={monitor.monitor_id}
                          className="flex items-center gap-2 rounded-md border p-2 text-sm"
                        >
                          <Checkbox
                            checked={checked}
                            disabled={started}
                            onCheckedChange={(next) =>
                              field.onChange(
                                next
                                  ? [...field.value, monitor.monitor_id]
                                  : field.value.filter(
                                      (id) => id !== monitor.monitor_id
                                    )
                              )
                            }
                          />
                          <span className="[overflow-wrap:anywhere]">
                            {monitor.display_name}
                          </span>
                        </label>
                      )
                    })}
                  </div>
                  <FormMessage />
                </FormItem>
              )}
            />
            <FormField
              control={form.control}
              name="description"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Description (optional)</FormLabel>
                  <FormControl>
                    <Textarea
                      rows={3}
                      placeholder="The API may be read-only for a few minutes while we switch databases."
                      {...field}
                    />
                  </FormControl>
                  <FormDescription>Shown on the public page.</FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => onOpenChange(false)}
              >
                Close
              </Button>
              <Button type="submit" disabled={pending}>
                {pending
                  ? 'Saving...'
                  : editing
                    ? 'Save'
                    : 'Schedule maintenance'}
              </Button>
            </DialogFooter>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  )
}
