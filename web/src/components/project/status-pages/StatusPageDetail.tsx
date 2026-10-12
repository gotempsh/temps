// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  deleteStatusPageMutation,
  getStatusPageOptions,
  listMonitorsOptions,
  publishStatusPageMutation,
  rotateStatusPageUrlMutation,
  unpublishStatusPageMutation,
  updateStatusPageMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  MonitorResponse,
  ProjectResponse,
  StatusPageResponse,
} from '@/api/client'
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
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { CopyButton } from '@/components/ui/copy-button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
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
import { Skeleton } from '@/components/ui/skeleton'
import { Textarea } from '@/components/ui/textarea'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ArrowLeft,
  ExternalLink,
  Eye,
  EyeOff,
  MoreVertical,
  RefreshCw,
  Trash2,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useFieldArray, useForm } from 'react-hook-form'
import { Link, useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'
import { StatusPageStateBadge } from './ProjectStatusPages'
import { StatusPageDomains } from './StatusPageDomains'
import { StatusPageIncidents } from './StatusPageIncidents'
import {
  formDefaults,
  operationQueries,
  statusPageFormSchema,
  toSelection,
  type StatusPageFormValues,
} from './status-page-form'

function DetailSkeleton() {
  return (
    <div className="w-full min-w-0 space-y-6">
      <Skeleton className="h-8 w-64" />
      <Skeleton className="h-20 w-full" />
      <Skeleton className="h-64 w-full" />
    </div>
  )
}

function PublicLinkSection({ page }: { page: StatusPageResponse }) {
  return (
    <section className="space-y-2">
      <h3 className="text-lg font-semibold">Public link</h3>
      <div className="flex min-w-0 flex-col gap-2 sm:flex-row sm:items-center">
        <code className="min-w-0 truncate rounded-md border bg-muted px-3 py-2 text-sm">
          {page.public_url}
        </code>
        <div className="flex gap-2">
          <CopyButton value={page.public_url} variant="outline" size="sm">
            Copy
          </CopyButton>
          {page.is_published && (
            <Button variant="outline" size="sm" asChild>
              <a href={page.public_url} target="_blank" rel="noreferrer">
                <ExternalLink className="size-4 sm:mr-2" />
                <span className="max-sm:sr-only">Open</span>
              </a>
            </Button>
          )}
        </div>
      </div>
      <p className="text-sm text-muted-foreground">
        {page.is_published
          ? 'Anyone with this link can see the page. Unpublish or rotate the link to revoke access immediately. Search engines are asked not to index it.'
          : 'This link returns “not found” until you publish the page.'}{' '}
        A machine-readable version is at{' '}
        <code className="text-xs">…/summary.json</code>.
      </p>
    </section>
  )
}

function StatusPageForm({
  page,
  monitors,
  projectSlug,
}: {
  page: StatusPageResponse
  monitors: MonitorResponse[]
  projectSlug: string
}) {
  const queryClient = useQueryClient()
  const form = useForm<StatusPageFormValues>({
    resolver: zodResolver(statusPageFormSchema),
    defaultValues: formDefaults(monitors, page),
  })
  const rows = useFieldArray({ control: form.control, name: 'monitors' })

  // Publishing, unpublishing or replacing the link refetches the page. Take
  // the fresh server values for fields the operator has not touched, but
  // never discard unsaved edits.
  useEffect(() => {
    form.reset(formDefaults(monitors, page), { keepDirtyValues: true })
  }, [form, monitors, page])

  const update = useMutation({
    ...updateStatusPageMutation(),
    meta: { errorTitle: 'Failed to save status page' },
    onSuccess: async (saved) => {
      // The saved page is the new clean baseline. The form is disabled while
      // a save is pending, so there are no newer edits for this to discard.
      form.reset(formDefaults(monitors, saved))
      await queryClient.invalidateQueries(operationQueries('getStatusPage'))
      await queryClient.invalidateQueries(operationQueries('listStatusPages'))
      toast.success(
        page.is_published
          ? 'Saved. The public page is updated.'
          : 'Saved. Publish when you are ready.'
      )
    },
  })

  const onSubmit = (values: StatusPageFormValues) => {
    update.mutate({
      path: { status_page_id: page.id },
      body: {
        title: values.title,
        description: values.description,
        monitors: toSelection(values.monitors),
      },
    })
  }

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(onSubmit)}>
        <fieldset
          className="min-w-0 space-y-6"
          disabled={update.isPending}
          aria-busy={update.isPending}
        >
          <section className="space-y-4">
            <h3 className="text-lg font-semibold">Details</h3>
            <FormField
              control={form.control}
              name="title"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Public title</FormLabel>
                  <FormControl>
                    <Input {...field} />
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
                  <FormLabel>Description (optional)</FormLabel>
                  <FormControl>
                    <Textarea
                      rows={2}
                      placeholder="Live status of the Acme API and dashboard."
                      {...field}
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
          </section>

          <section className="space-y-3">
            <div>
              <h3 className="text-lg font-semibold">Monitors</h3>
              <p className="text-sm text-muted-foreground">
                Tick the monitors to show and give each a public name. Only that
                name, its current state and its daily uptime are published —
                never the monitor&apos;s internal name, URL, host or error
                messages.
              </p>
            </div>
            {rows.fields.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                This project has no monitors.{' '}
                <Link
                  className="underline underline-offset-4"
                  to={`/projects/${projectSlug}/monitors`}
                >
                  Create one
                </Link>{' '}
                to show it here.
              </p>
            ) : (
              <Card>
                <CardContent className="divide-y p-0">
                  {rows.fields.map((row, index) => (
                    <MonitorSelectionRow
                      key={row.id}
                      index={index}
                      form={form}
                    />
                  ))}
                </CardContent>
              </Card>
            )}
            <FormField
              control={form.control}
              name="monitors"
              render={() => (
                <FormItem>
                  <FormMessage />
                </FormItem>
              )}
            />
          </section>

          <div className="flex justify-end">
            <Button type="submit" disabled={update.isPending}>
              {update.isPending ? 'Saving...' : 'Save changes'}
            </Button>
          </div>
        </fieldset>
      </form>
    </Form>
  )
}

function MonitorSelectionRow({
  index,
  form,
}: {
  index: number
  form: ReturnType<typeof useForm<StatusPageFormValues>>
}) {
  const selected = form.watch(`monitors.${index}.selected`)
  const internalName = form.watch(`monitors.${index}.monitor_name`)
  return (
    <div className="flex flex-col gap-3 p-4 sm:flex-row sm:items-start">
      <FormField
        control={form.control}
        name={`monitors.${index}.selected`}
        render={({ field }) => (
          <FormItem className="flex min-w-0 flex-1 flex-row items-center gap-3 space-y-0">
            <FormControl>
              <Checkbox
                checked={field.value}
                onCheckedChange={(checked) => field.onChange(checked === true)}
                aria-label={`Show ${internalName} on the status page`}
              />
            </FormControl>
            <div className="min-w-0">
              <FormLabel className="truncate font-medium">
                {internalName}
              </FormLabel>
              <FormDescription className="text-xs">
                Internal name — not published
              </FormDescription>
            </div>
          </FormItem>
        )}
      />
      <FormField
        control={form.control}
        name={`monitors.${index}.display_name`}
        render={({ field }) => (
          <FormItem className="w-full sm:w-72">
            <FormControl>
              <Input
                placeholder="Public name, e.g. API"
                disabled={!selected}
                aria-label={`Public name for ${internalName}`}
                {...field}
              />
            </FormControl>
            <FormMessage />
          </FormItem>
        )}
      />
    </div>
  )
}

export function StatusPageDetail({ project }: { project: ProjectResponse }) {
  const { statusPageId } = useParams()
  const id = Number(statusPageId)
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [confirmDelete, setConfirmDelete] = useState(false)
  const [confirmRotate, setConfirmRotate] = useState(false)

  const pageQuery = useQuery({
    ...getStatusPageOptions({ path: { status_page_id: id } }),
    enabled: Number.isFinite(id),
  })
  const monitorsQuery = useQuery(
    listMonitorsOptions({ path: { project_id: project.id } })
  )

  const refresh = async () => {
    await queryClient.invalidateQueries(operationQueries('getStatusPage'))
    await queryClient.invalidateQueries(operationQueries('listStatusPages'))
    // Unpublishing releases every custom domain's certificate, so their
    // state changes too; a stale "Serving" row would hide Verify.
    await queryClient.invalidateQueries(
      operationQueries('listStatusPageDomains')
    )
  }
  const publish = useMutation({
    ...publishStatusPageMutation(),
    meta: { errorTitle: 'Failed to publish status page' },
    onSuccess: async () => {
      await refresh()
      toast.success('Published. The public link is live.')
    },
  })
  const unpublish = useMutation({
    ...unpublishStatusPageMutation(),
    meta: { errorTitle: 'Failed to unpublish status page' },
    onSuccess: async () => {
      await refresh()
      toast.success('Unpublished. The public link now returns “not found”.')
    },
  })
  const rotate = useMutation({
    ...rotateStatusPageUrlMutation(),
    meta: { errorTitle: 'Failed to replace the public link' },
    onSuccess: async () => {
      await refresh()
      setConfirmRotate(false)
      toast.success('New public link created. The old one no longer works.')
    },
  })
  const remove = useMutation({
    ...deleteStatusPageMutation(),
    meta: { errorTitle: 'Failed to delete status page' },
    onSuccess: async () => {
      await queryClient.invalidateQueries(operationQueries('listStatusPages'))
      toast.success('Status page deleted.')
      navigate(`/projects/${project.slug}/status-pages`)
    },
  })

  if (pageQuery.isError || monitorsQuery.isError) {
    const error = pageQuery.error ?? monitorsQuery.error
    return (
      <ErrorAlert
        title="Failed to load status page"
        description={
          error instanceof Error
            ? error.message
            : 'An unexpected error occurred'
        }
        retry={() => {
          pageQuery.refetch()
          monitorsQuery.refetch()
        }}
      />
    )
  }
  if (!pageQuery.data || !monitorsQuery.data) {
    return <DetailSkeleton />
  }

  const page = pageQuery.data
  const path = { path: { status_page_id: page.id } }
  const canPublish = page.monitors.length > 0

  return (
    <div className="w-full min-w-0 space-y-6">
      <Link
        to={`/projects/${project.slug}/status-pages`}
        className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
      >
        <ArrowLeft className="size-4" />
        Status pages
      </Link>

      <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0 space-y-1">
          <div className="flex flex-wrap items-center gap-2">
            <h2 className="truncate text-2xl font-bold tracking-tight">
              {page.title}
            </h2>
            <StatusPageStateBadge page={page} />
          </div>
          {!canPublish && (
            <p className="text-sm text-muted-foreground">
              Select at least one monitor and save before publishing.
            </p>
          )}
        </div>
        <div className="flex gap-2">
          {page.is_published ? (
            <Button
              variant="outline"
              disabled={unpublish.isPending}
              onClick={() => unpublish.mutate(path)}
            >
              <EyeOff className="size-4 sm:mr-2" />
              <span className="max-sm:sr-only">
                {unpublish.isPending ? 'Unpublishing...' : 'Unpublish'}
              </span>
            </Button>
          ) : (
            <Button
              disabled={!canPublish || publish.isPending}
              onClick={() => publish.mutate(path)}
            >
              <Eye className="size-4 sm:mr-2" />
              <span className="max-sm:sr-only">
                {publish.isPending ? 'Publishing...' : 'Publish'}
              </span>
            </Button>
          )}
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="outline" size="icon" aria-label="More actions">
                <MoreVertical className="size-4" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onSelect={() => setConfirmRotate(true)}>
                <RefreshCw className="mr-2 size-4" />
                Replace public link
              </DropdownMenuItem>
              <DropdownMenuItem
                className="text-destructive"
                onSelect={() => setConfirmDelete(true)}
              >
                <Trash2 className="mr-2 size-4" />
                Delete
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>

      <PublicLinkSection page={page} />

      <StatusPageDomains page={page} />

      <StatusPageForm
        page={page}
        monitors={monitorsQuery.data}
        projectSlug={project.slug}
      />

      {/* Keyed by project: switching projects must never show (and let you
          act on) the previous project's incidents or page number. */}
      <StatusPageIncidents key={project.id} project={project} />

      <AlertDialog open={confirmRotate} onOpenChange={setConfirmRotate}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Replace the public link?</AlertDialogTitle>
            <AlertDialogDescription>
              The current link stops working immediately. Anyone you shared it
              with will need the new one.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction onClick={() => rotate.mutate(path)}>
              {rotate.isPending ? 'Replacing...' : 'Replace link'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog open={confirmDelete} onOpenChange={setConfirmDelete}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete “{page.title}”?</AlertDialogTitle>
            <AlertDialogDescription>
              The public link stops working immediately. Monitors and incidents
              are not affected.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive hover:bg-destructive/90"
              onClick={() => remove.mutate(path)}
            >
              {remove.isPending ? 'Deleting...' : 'Delete'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
