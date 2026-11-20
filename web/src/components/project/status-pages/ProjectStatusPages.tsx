// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { listStatusPagesOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse, StatusPageResponse } from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { RecordLink, RecordRow } from '@temps-sdk/ds'
import { Button } from '@/components/ui/button'
import { CreateActionButton } from '@/components/ui/create-action-button'
import { EmptyState } from '@/components/ui/empty-state'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
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
import { useQuery } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import { Globe, Plus } from 'lucide-react'
import { useState } from 'react'
import { Link, Navigate, useParams } from 'react-router'
import { PublicLinkCell } from './StatusPagePublicLink'
import { newStatusPageUrl, statusPageUrl } from './status-page-form'

const PAGE_SIZE = 20

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
  const createUrl = newStatusPageUrl(project?.id)
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
          <CreateActionButton to={createUrl} label="Create status page" />
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
            <Button asChild>
              <Link to={createUrl}>
                <Plus className="mr-2 size-4" />
                Create status page
              </Link>
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
                <RecordRow key={item.id}>
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
                </RecordRow>
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
    </div>
  )
}

/**
 * Status pages used to be a project tab. They are platform-wide now, so the
 * old project URL sends links and bookmarks to the platform list.
 */
export function LegacyStatusPagesRedirect() {
  return <Navigate to="/status-pages" replace />
}

/**
 * Pages used to live under their project. Keep those links and bookmarks
 * working by sending them to the page's platform URL.
 */
export function LegacyStatusPageRedirect() {
  const { statusPageId } = useParams()
  return <Navigate to={`/status-pages/${statusPageId ?? ''}`} replace />
}
