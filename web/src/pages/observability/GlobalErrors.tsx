// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { number } from '@/lib/global-observability'
import { useGlobalView } from '@/hooks/useGlobalView'
import { Link } from 'react-router'
import {
  listGlobalErrorGroupsOptions,
  listGlobalErrorGroupsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { queryKeyPrefix } from '@/lib/refresh'
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
import { FixErrorWithAgentButton } from '@/components/ai/FixErrorWithAgentButton'
import { offersFixInList } from '@/components/ai/fix-error-with-agent'
import { useGlobalPageInRange } from '@/hooks/useGlobalView'
import { useWindowQuery } from '@/hooks/useWindowQuery'

export default function GlobalErrors() {
  const view = useGlobalView()
  const status = ['unresolved', 'resolved', 'ignored'].includes(
    view.params.get('status') ?? ''
  )
    ? view.params.get('status')!
    : 'all'
  const query = useWindowQuery({
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
  useGlobalPageInRange(
    view,
    query.data && !query.isPlaceholderData
      ? {
          rows: query.data.data.length,
          total: query.data.pagination.total_count,
        }
      : undefined
  )
  return (
    <GlobalPage
      title="Errors"
      description="Review application issues across your services. Event and user counts cover the selected time range."
      view={view}
      refreshQueryKeys={[queryKeyPrefix(listGlobalErrorGroupsQueryKey)]}
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
                <TableHead className="hidden md:table-cell">Service</TableHead>
                <TableHead>Status</TableHead>
                <TableHead className="text-right">Events</TableHead>
                <TableHead className="hidden md:table-cell text-right">
                  Users
                </TableHead>
                <TableHead className="hidden md:table-cell">
                  Last seen
                </TableHead>
                <TableHead>
                  <span className="sr-only">Actions</span>
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
                      className="line-clamp-2 font-medium break-words hover:underline"
                      title={issue.title}
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
                    {/* Users are linked visitors; a server-side error with
                        none says nothing about how many people it hit. */}
                    {issue.affected_users > 0 ? (
                      number(issue.affected_users)
                    ) : (
                      <span
                        className="text-muted-foreground"
                        title="No visitor is linked to these events"
                      >
                        —
                      </span>
                    )}
                  </TableCell>
                  <TableCell className="hidden md:table-cell">
                    <TimeAgo date={issue.last_seen} />
                  </TableCell>
                  <TableCell className="text-right">
                    {/* Every error opens Agents in its own service, so the
                        agent reads the right project's code and events. */}
                    {offersFixInList(issue.status) && (
                      <FixErrorWithAgentButton
                        error={{
                          id: issue.id,
                          title: issue.title,
                          error_type: issue.error_type,
                          environment_name: issue.environment_name,
                          project: {
                            id: issue.project_id,
                            slug: issue.project_slug,
                            name: issue.project_name,
                          },
                        }}
                      />
                    )}
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
