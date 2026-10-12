// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createStatusPageMutation,
  listMonitorsOptions,
  listStatusPagesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse, StatusPageResponse } from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
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
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
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
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { Link, useNavigate } from 'react-router'
import { toast } from 'sonner'
import * as z from 'zod'
import { resolvePublicUrl } from './public-url'
import { MAX_TITLE_CHARS, operationQueries } from './status-page-form'

const createSchema = z.object({
  title: z
    .string()
    .trim()
    .min(1, 'Title is required')
    .max(MAX_TITLE_CHARS, `At most ${MAX_TITLE_CHARS} characters`),
})

type CreateValues = z.infer<typeof createSchema>

export function StatusPageStateBadge({ page }: { page: StatusPageResponse }) {
  return page.is_published ? (
    <Badge variant="success-subtle">Published</Badge>
  ) : (
    <Badge variant="neutral-subtle">Draft</Badge>
  )
}

function CreateStatusPageDialog({
  project,
  open,
  onOpenChange,
  hasMonitors,
}: {
  project: ProjectResponse
  open: boolean
  onOpenChange: (open: boolean) => void
  hasMonitors: boolean
}) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const form = useForm<CreateValues>({
    resolver: zodResolver(createSchema),
    defaultValues: { title: `${project.name} status` },
  })
  const create = useMutation({
    ...createStatusPageMutation(),
    meta: { errorTitle: 'Failed to create status page' },
    onSuccess: async (page) => {
      await queryClient.invalidateQueries(operationQueries('listStatusPages'))
      toast.success('Status page created. Choose the monitors it shows.')
      onOpenChange(false)
      form.reset()
      navigate(`/projects/${project.slug}/status-pages/${page.id}`)
    },
  })

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Create status page</DialogTitle>
          <DialogDescription>
            A public page your users can open to check whether your services are
            up. It starts as a draft: you pick which monitors it shows and what
            they are called before publishing.
          </DialogDescription>
        </DialogHeader>
        {!hasMonitors && (
          <p className="text-sm text-muted-foreground">
            This project has no monitors yet, so there is nothing to show.{' '}
            <Link
              className="underline underline-offset-4"
              to={`/projects/${project.slug}/monitors`}
            >
              Create a monitor
            </Link>{' '}
            first, or create the draft now and add monitors later.
          </p>
        )}
        <Form {...form}>
          <form
            className="space-y-4"
            onSubmit={form.handleSubmit((values) =>
              create.mutate({
                path: { project_id: project.id },
                body: { title: values.title, monitors: [] },
              })
            )}
          >
            <FormField
              control={form.control}
              name="title"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Public title</FormLabel>
                  <FormControl>
                    <Input placeholder="Acme status" {...field} />
                  </FormControl>
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
                Cancel
              </Button>
              <Button type="submit" disabled={create.isPending}>
                {create.isPending ? 'Creating...' : 'Create draft'}
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

export function ProjectStatusPages({ project }: { project: ProjectResponse }) {
  const [createOpen, setCreateOpen] = useState(false)
  const pages = useQuery(
    listStatusPagesOptions({
      path: { project_id: project.id },
      query: { page: 1, page_size: 100 },
    })
  )
  const monitors = useQuery(
    listMonitorsOptions({ path: { project_id: project.id } })
  )
  const items = pages.data?.items ?? []
  const hasMonitors = (monitors.data?.length ?? 0) > 0

  return (
    <div className="w-full min-w-0 space-y-6">
      <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0">
          <h2 className="text-2xl font-bold tracking-tight">Status pages</h2>
          <p className="max-w-2xl text-muted-foreground">
            Publish the state and 90-day uptime of selected monitors at a
            private, unguessable public link. Only the names you choose are
            shown — never URLs, hostnames or error details.
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
          title="No status pages yet"
          description="Show customers whether your services are up, e.g. “API — Operational, 99.98% uptime over 90 days”, without exposing anything internal."
          action={
            <Button onClick={() => setCreateOpen(true)}>
              <Plus className="mr-2 size-4" />
              Create status page
            </Button>
          }
        />
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Title</TableHead>
              <TableHead>State</TableHead>
              <TableHead className="hidden sm:table-cell">Monitors</TableHead>
              <TableHead className="hidden md:table-cell">
                Public link
              </TableHead>
              <TableHead className="hidden lg:table-cell">Created</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {items.map((page) => (
              <TableRow key={page.id}>
                <TableCell className="min-w-0 font-medium">
                  <Link
                    className="hover:underline"
                    to={`/projects/${project.slug}/status-pages/${page.id}`}
                  >
                    {page.title}
                  </Link>
                </TableCell>
                <TableCell>
                  <StatusPageStateBadge page={page} />
                </TableCell>
                <TableCell className="hidden sm:table-cell">
                  {page.monitors.length}
                </TableCell>
                <TableCell className="hidden md:table-cell">
                  {page.is_published ? (
                    <CopyButton
                      value={resolvePublicUrl(
                        page.public_url,
                        window.location.origin
                      )}
                      variant="ghost"
                      size="sm"
                      label="Copy public link"
                    >
                      Copy link
                    </CopyButton>
                  ) : (
                    <span className="text-sm text-muted-foreground">
                      Not public
                    </span>
                  )}
                </TableCell>
                <TableCell className="hidden text-muted-foreground lg:table-cell">
                  {formatDistanceToNow(new Date(page.created_at), {
                    addSuffix: true,
                  })}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}

      <CreateStatusPageDialog
        project={project}
        open={createOpen}
        onOpenChange={setCreateOpen}
        hasMonitors={hasMonitors}
      />
    </div>
  )
}
