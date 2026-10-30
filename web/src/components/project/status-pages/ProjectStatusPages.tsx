// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createStatusPageMutation,
  listStatusPagesOptions,
  publishStatusPageMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse, StatusPageResponse } from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { RecordLink } from '@temps-sdk/ds'
import { Button } from '@/components/ui/button'
import { CreateActionButton } from '@/components/ui/create-action-button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { EmptyState } from '@/components/ui/empty-state'
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
import { Switch } from '@/components/ui/switch'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import { Globe, Plus } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { Link, Navigate, useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'
import { StatusPageComponentsEditor } from './StatusPageComponentsEditor'
import { PublicLinkCell } from './StatusPagePublicLink'
import {
  createDefaults,
  createStatusPageSchema,
  operationQueries,
  toSelection,
  type CreateStatusPageValues,
} from './status-page-form'

const PAGE_SIZE = 20

/** Console URL of a status page. Pages are platform-level, not per project. */
function statusPageUrl(id: number) {
  return `/status-pages/${id}`
}

export function StatusPageStateBadge({ page }: { page: StatusPageResponse }) {
  return page.is_published ? (
    <Badge variant="success-subtle">Published</Badge>
  ) : (
    <Badge variant="neutral-subtle">Draft</Badge>
  )
}

/** The projects a page draws its components from, as links. */
export function StatusPageProjects({
  page,
  max = 3,
}: {
  page: Pick<StatusPageResponse, 'projects'>
  max?: number
}) {
  if (page.projects.length === 0) {
    return <span className="text-muted-foreground">No components</span>
  }
  const shown = page.projects.slice(0, max)
  const rest = page.projects.length - shown.length
  return (
    <span className="inline-flex flex-wrap items-center gap-1">
      {shown.map((p) => (
        <Link key={p.id} to={`/projects/${p.slug}`}>
          <Badge variant="neutral-subtle" className="hover:bg-accent">
            {p.name}
          </Badge>
        </Link>
      ))}
      {rest > 0 && (
        <span className="text-xs text-muted-foreground">+{rest} more</span>
      )}
    </span>
  )
}

export function CreateStatusPageDialog({
  initialProject,
  open,
  onOpenChange,
}: {
  /** The project the user came from: its picker opens on it. */
  initialProject?: Pick<ProjectResponse, 'id' | 'name'>
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const form = useForm<CreateStatusPageValues>({
    resolver: zodResolver(createStatusPageSchema),
    defaultValues: createDefaults(initialProject?.name ?? '', []),
  })
  const publishOnCreate = useWatch({ control: form.control, name: 'publish' })
  // Start from a clean form each time the dialog opens.
  useEffect(() => {
    if (open) form.reset(createDefaults(initialProject?.name ?? '', []))
  }, [open, form, initialProject?.name])

  const publish = useMutation({
    ...publishStatusPageMutation(),
    meta: { errorTitle: 'Created, but publishing failed' },
  })
  const create = useMutation({
    ...createStatusPageMutation(),
    meta: { errorTitle: 'Failed to create status page' },
    onSuccess: async (page, variables) => {
      let published = false
      if (form.getValues('publish')) {
        try {
          await publish.mutateAsync({ path: { status_page_id: page.id } })
          published = true
        } catch {
          // Reported by the mutation's error toast; the draft still exists.
        }
      }
      await queryClient.invalidateQueries(operationQueries('listStatusPages'))
      toast.success(
        published
          ? 'Status page is live. Share the public link from the overview.'
          : `Draft created with ${variables.body.monitors.length} component(s). Publish when ready.`
      )
      onOpenChange(false)
      navigate(statusPageUrl(page.id))
    },
  })
  const busy = create.isPending || publish.isPending

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>Create status page</DialogTitle>
          <DialogDescription>
            Add the services your users care about, from any of your projects,
            and group them (e.g. “Cloud → API, Dashboard”). Each one shows its
            live state, 90-day uptime and response times under the public name
            you choose — never its URL or errors.
          </DialogDescription>
        </DialogHeader>
        <Form {...form}>
          <form
            className="space-y-5"
            onSubmit={form.handleSubmit((values) =>
              create.mutate({
                body: {
                  title: values.title,
                  monitors: toSelection(values.components),
                },
              })
            )}
          >
            <FormField
              control={form.control}
              name="title"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Page title</FormLabel>
                  <FormControl>
                    <Input placeholder="Acme status" {...field} />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="components"
              render={({ field, fieldState }) => (
                <FormItem>
                  <FormLabel>Components</FormLabel>
                  <StatusPageComponentsEditor
                    value={field.value}
                    onChange={field.onChange}
                    errors={form.formState.errors.components}
                    initialProjectId={initialProject?.id}
                  />
                  {fieldState.error?.message && (
                    <p className="text-sm font-medium text-destructive">
                      {fieldState.error.message}
                    </p>
                  )}
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="publish"
              render={({ field }) => (
                <FormItem className="flex flex-row items-center justify-between gap-4 space-y-0 rounded-lg border p-3">
                  <div>
                    <FormLabel>Publish right away</FormLabel>
                    <FormDescription>
                      Off creates a draft you can preview and style first.
                    </FormDescription>
                  </div>
                  <FormControl>
                    <Switch
                      checked={field.value}
                      onCheckedChange={field.onChange}
                    />
                  </FormControl>
                </FormItem>
              )}
            />

            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => onOpenChange(false)}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={busy}>
                {busy
                  ? 'Creating...'
                  : publishOnCreate
                    ? 'Create and publish'
                    : 'Create draft'}
              </Button>
            </DialogFooter>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  )
}

function StatusPagesSkeleton() {
  return (
    <div className="space-y-2">
      {Array.from({ length: 3 }).map((_, i) => (
        <Skeleton key={i} className="h-12 w-full" />
      ))}
    </div>
  )
}

/**
 * Status pages, all of them or those showing one project's components. Pages
 * are platform-level: one page can show services from many projects.
 */
export function StatusPagesList({ project }: { project?: ProjectResponse }) {
  const [createOpen, setCreateOpen] = useState(false)
  const [page, setPage] = useState(1)
  const pages = useQuery(
    listStatusPagesOptions({
      query: { page, page_size: PAGE_SIZE, project_id: project?.id },
    })
  )
  const items = pages.data?.items ?? []
  const total = pages.data?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / PAGE_SIZE))

  // Deleting the last page's only item leaves you past the end: step back.
  if (pages.data && page > totalPages) setPage(totalPages)

  return (
    <div className="w-full min-w-0 space-y-6">
      <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0">
          <h2 className="text-2xl font-bold tracking-tight">Status pages</h2>
          <p className="max-w-2xl text-muted-foreground">
            {project
              ? `Public pages that show ${project.name}. A page can also show services from your other projects.`
              : 'Public pages like status.example.com showing the live state, 90-day uptime and response times of the services you pick, from any project, plus the incidents you post.'}{' '}
            Only the names you choose are shown — never URLs, hostnames or error
            details.
          </p>
        </div>
        {items.length > 0 && (
          <CreateActionButton
            onClick={() => setCreateOpen(true)}
            label="Create status page"
          />
        )}
      </div>

      {pages.isError ? (
        <ErrorAlert
          title="Failed to load status pages"
          description={
            pages.error instanceof Error
              ? pages.error.message
              : 'An unexpected error occurred'
          }
          retry={() => pages.refetch()}
        />
      ) : pages.isLoading ? (
        <StatusPagesSkeleton />
      ) : items.length === 0 ? (
        <EmptyState
          size="compact"
          icon={Globe}
          title={
            project
              ? `No status page shows ${project.name} yet`
              : 'No status pages yet'
          }
          description="Show customers whether your services are up, e.g. “API — Operational, 99.98% uptime over 90 days”, without exposing anything internal."
          action={
            <Button onClick={() => setCreateOpen(true)}>
              <Plus className="mr-2 size-4" />
              Create status page
            </Button>
          }
        />
      ) : (
        <div className="space-y-4">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Title</TableHead>
                <TableHead>State</TableHead>
                <TableHead className="hidden md:table-cell">Projects</TableHead>
                <TableHead className="hidden sm:table-cell">
                  Components
                </TableHead>
                <TableHead className="hidden lg:table-cell">
                  Public link
                </TableHead>
                <TableHead className="hidden xl:table-cell">Created</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {items.map((item) => (
                <TableRow key={item.id}>
                  <TableCell className="min-w-0 font-medium">
                    <RecordLink to={statusPageUrl(item.id)}>
                      {item.title}
                    </RecordLink>
                  </TableCell>
                  <TableCell>
                    <StatusPageStateBadge page={item} />
                  </TableCell>
                  <TableCell className="hidden md:table-cell">
                    <StatusPageProjects page={item} />
                  </TableCell>
                  <TableCell className="hidden sm:table-cell">
                    {item.monitors.length}
                  </TableCell>
                  <TableCell className="hidden lg:table-cell">
                    <PublicLinkCell page={item} />
                  </TableCell>
                  <TableCell className="hidden text-muted-foreground xl:table-cell">
                    {formatDistanceToNow(new Date(item.created_at), {
                      addSuffix: true,
                    })}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
          {totalPages > 1 && (
            <ResponsivePagination
              page={page}
              pageSize={PAGE_SIZE}
              total={total}
              totalPages={totalPages}
              onPageChange={setPage}
              ariaLabel="Status pages pagination"
            />
          )}
        </div>
      )}

      <CreateStatusPageDialog
        initialProject={project}
        open={createOpen}
        onOpenChange={setCreateOpen}
      />
    </div>
  )
}

/** A project's Status pages tab: the platform pages that include it. */
export function ProjectStatusPages({ project }: { project: ProjectResponse }) {
  return <StatusPagesList project={project} />
}

/**
 * Pages used to live under their project. Keep those links and bookmarks
 * working by sending them to the page's platform URL.
 */
export function LegacyStatusPageRedirect() {
  const { statusPageId } = useParams()
  return <Navigate to={`/status-pages/${statusPageId ?? ''}`} replace />
}
