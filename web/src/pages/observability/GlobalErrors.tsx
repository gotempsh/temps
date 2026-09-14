// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { number } from '@/lib/global-observability'
import { useGlobalView } from '@/hooks/useGlobalView'
import { Link } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { listGlobalErrorGroupsOptions } from '@/api/client/@tanstack/react-query.gen'
import {
  GlobalPage,
  GlobalPagination,
  QueryContent,
  FilterSelect,
} from '@/components/observability/GlobalPage'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { Badge } from '@/components/ui/badge'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { OBSERVABILITY_PAGE_SIZE } from '@/lib/global-observability'

export default function GlobalErrors() {
  const view = useGlobalView()
  const status = ['unresolved', 'resolved', 'ignored'].includes(
    view.params.get('status') ?? ''
  )
    ? view.params.get('status')!
    : 'all'
  const query = useQuery({
    ...listGlobalErrorGroupsOptions({
      query: {
        project_id: view.projectId,
        start_date: view.from,
        end_date: view.to,
        search: view.search || undefined,
        status: status === 'all' ? undefined : status,
        page: view.page,
        page_size: OBSERVABILITY_PAGE_SIZE,
      },
    }),
    retry: false,
  })
  return (
    <GlobalPage
      title="Errors"
      description="Review application issues across your projects. Event and user counts cover the selected time range."
      view={view}
      fetching={query.isFetching}
      refresh={() => void query.refetch()}
      searchLabel="Search errors"
      filters={
        <FilterSelect
          label="Error status"
          value={status}
          onChange={(status) =>
            view.patch({ status: status === 'all' ? undefined : status })
          }
          options={[
            ['all', 'All statuses'],
            ['unresolved', 'Unresolved'],
            ['resolved', 'Resolved'],
            ['ignored', 'Ignored'],
          ]}
        />
      }
    >
      <QueryContent
        title="Errors"
        loading={query.isPending}
        error={query.error}
        empty={!query.data?.data.length}
        retry={() => void query.refetch()}
      >
        <div className="rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Issue</TableHead>
                <TableHead className="hidden md:table-cell">Project</TableHead>
                <TableHead>Status</TableHead>
                <TableHead className="text-right">Events</TableHead>
                <TableHead className="hidden md:table-cell text-right">
                  Users
                </TableHead>
                <TableHead className="hidden md:table-cell">
                  Last seen
                </TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {query.data?.data.map((issue) => (
                <TableRow key={issue.id}>
                  <TableCell className="max-w-sm">
                    <p className="text-xs text-muted-foreground md:hidden">
                      {issue.project_name}
                    </p>
                    <Link
                      className="font-medium hover:underline break-words"
                      to={`/projects/${encodeURIComponent(issue.project_slug)}/errors/${issue.id}`}
                    >
                      {issue.title}
                    </Link>
                    <p className="text-xs text-muted-foreground">
                      {issue.error_type} · #{issue.id}
                      {issue.environment_name
                        ? ` · ${issue.environment_name}`
                        : ''}
                    </p>
                  </TableCell>
                  <TableCell className="hidden md:table-cell">
                    {issue.project_name}
                  </TableCell>
                  <TableCell>
                    <Badge
                      variant={
                        issue.status === 'unresolved'
                          ? 'destructive'
                          : 'secondary'
                      }
                    >
                      {issue.status}
                    </Badge>
                  </TableCell>
                  <TableCell className="text-right tabular-nums">
                    {number(issue.events_in_range)}
                  </TableCell>
                  <TableCell className="hidden md:table-cell text-right tabular-nums">
                    {number(issue.affected_users)}
                  </TableCell>
                  <TableCell className="hidden md:table-cell">
                    <TimeAgo date={issue.last_seen} />
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
        <GlobalPagination
          view={view}
          total={query.data?.pagination.total_count ?? 0}
        />
      </QueryContent>
    </GlobalPage>
  )
}
