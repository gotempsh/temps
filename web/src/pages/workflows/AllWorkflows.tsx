// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useInfiniteQuery } from '@tanstack/react-query'
import { Ledger, PageState, Status } from '@temps-sdk/ds'
import { Workflow } from 'lucide-react'
import { Link, useSearchParams } from 'react-router'
import { listAllWorkflowRuns } from '@/api/client/sdk.gen'
import type { WorkflowRunStatus, WorkflowRunResponse } from '@/api/client'
import { RefreshButton } from '@/components/ui/refresh-button'
import { Button } from '@/components/ui/button'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { usePageTitle } from '@/hooks/usePageTitle'
import { problemDetail } from '@/lib/api-problem'
import { runHref, workflowStatuses, workflowTone } from './status'

export function AllWorkflows() {
  usePageTitle('All workflows')
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => {
    setBreadcrumbs([{ label: 'Workflows' }])
    return () => setBreadcrumbs([])
  }, [setBreadcrumbs])
  const [search, setSearch] = useSearchParams()
  const selected = search.get('status')
  const status = workflowStatuses.includes(selected as WorkflowRunStatus)
    ? (selected as WorkflowRunStatus)
    : undefined
  const queryKey = ['all-workflow-runs', status]
  const query = useInfiniteQuery({
    queryKey,
    initialPageParam: undefined as string | undefined,
    queryFn: async ({ pageParam, signal }) => {
      const response = await listAllWorkflowRuns({
        query: { status, cursor: pageParam, page_size: 20 },
        signal,
        throwOnError: true,
      })
      return response.data
    },
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  })
  const rows = query.data?.pages.flatMap((page) => page.runs) ?? []
  return (
    <Ledger<WorkflowRunResponse>
      title="All workflows"
      actions={<RefreshButton queryKeys={[queryKey]} />}
      toolbar={
        <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap">
          <Select
            value={status ?? 'all'}
            onValueChange={(value) =>
              setSearch(value === 'all' ? {} : { status: value })
            }
          >
            <SelectTrigger className="w-full sm:w-48" aria-label="Run status">
              <SelectValue placeholder="All states" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All states</SelectItem>
              {workflowStatuses.map((state) => (
                <SelectItem key={state} value={state}>
                  {state}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          {query.hasNextPage && (
            <Button
              variant="outline"
              aria-disabled={query.isFetchingNextPage}
              onClick={() => {
                if (!query.isFetchingNextPage) void query.fetchNextPage()
              }}
            >
              {query.isFetchingNextPage ? 'Loading runs…' : 'Load more runs'}
            </Button>
          )}
          {query.isError && (
            <p role="alert" className="text-sm text-destructive">
              {problemDetail(
                query.error,
                'Could not load workflow runs. Refresh to retry.'
              )}
            </p>
          )}
        </div>
      }
      columns={[
        {
          key: 'workflow',
          header: 'Workflow',
          render: (run) => (
            <div className="min-w-0">
              <Link
                className="font-medium underline-offset-4 hover:underline [overflow-wrap:anywhere]"
                to={runHref(run)}
              >
                {run.workflow}
              </Link>
              <p className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
                Project {run.project_id} · Environment {run.environment_id}
              </p>
            </div>
          ),
        },
        {
          key: 'status',
          header: 'State',
          render: (run) => (
            <Status tone={workflowTone(run.status)} label={run.status} />
          ),
        },
        {
          key: 'created',
          header: 'Started',
          className: 'hidden @xl:table-cell',
          render: (run) => new Date(run.created_at).toLocaleString(),
        },
        {
          key: 'steps',
          header: 'Steps',
          className: 'hidden @xl:table-cell',
          render: (run) => run.step_count,
        },
      ]}
      rows={rows}
      rowKey={(run) => run.id}
      isLoading={query.isPending}
      empty={
        <PageState
          variant={query.isError ? 'failed' : 'empty'}
          size="compact"
          icon={Workflow}
          title={
            query.isError
              ? 'Could not load workflows'
              : status
                ? 'No matching runs'
                : 'No workflow runs yet'
          }
          description={
            query.isError
              ? 'Refresh to try again.'
              : 'Start a workflow in a project environment to see its progress here.'
          }
          action={
            <Button asChild variant="outline">
              <Link to="/projects">Open projects</Link>
            </Button>
          }
        />
      }
    />
  )
}
