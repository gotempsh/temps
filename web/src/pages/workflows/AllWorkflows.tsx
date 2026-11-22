// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import {
  useInfiniteQuery,
  useQuery,
  type UseQueryResult,
} from '@tanstack/react-query'
import {
  DataTable,
  PageContainer,
  PageHeader,
  PageState,
  RecordLink,
  Status,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from '@temps-sdk/ds'
import { History, Workflow } from 'lucide-react'
import { Link, useSearchParams } from 'react-router'
import { listAllWorkflowRuns } from '@/api/client/sdk.gen'
import { listWorkflowDefinitionsOptions } from '@/api/client/@tanstack/react-query.gen'
import type {
  GlobalWorkflowRunResponse,
  WorkflowDefinitionResponse,
  WorkflowDefinitionsResponse,
  WorkflowRunStatus,
} from '@/api/client'
import { RefreshButton } from '@/components/ui/refresh-button'
import { Button } from '@/components/ui/button'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { usePageTitle } from '@/hooks/usePageTitle'
import { problemDetail } from '@/lib/api-problem'
import {
  definitionStatus,
  runHref,
  workflowHref,
  workflowStatuses,
  workflowTone,
} from './status'
import { WorkflowsSetupGuide } from './WorkflowsSetupGuide'

type View = 'workflows' | 'runs'

export function AllWorkflows() {
  usePageTitle('Workflows')
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => {
    setBreadcrumbs([{ label: 'Workflows' }])
    return () => setBreadcrumbs([])
  }, [setBreadcrumbs])
  const [search, setSearch] = useSearchParams()
  // A status filter only applies to runs, so a link carrying one opens them.
  const view: View =
    search.get('view') === 'runs' || search.has('status') ? 'runs' : 'workflows'
  const page = Math.max(1, Number(search.get('page')) || 1)
  const definitionsOptions = listWorkflowDefinitionsOptions({
    query: { page, page_size: 20 },
  })
  const definitions = useQuery(definitionsOptions)

  return (
    <PageContainer>
      <PageHeader
        title="Workflows"
        actions={
          <RefreshButton
            queryKeys={[definitionsOptions.queryKey, ['all-workflow-runs']]}
          />
        }
      />
      <Tabs
        value={view}
        onValueChange={(value) =>
          setSearch(value === 'runs' ? { view: 'runs' } : {})
        }
      >
        <TabsList>
          <TabsTrigger
            value="workflows"
            icon={Workflow}
            count={definitions.data?.total}
          >
            Workflows
          </TabsTrigger>
          <TabsTrigger value="runs" icon={History}>
            Runs
          </TabsTrigger>
        </TabsList>
        <TabsContent value="workflows" className="mt-4">
          <WorkflowDefinitions
            query={definitions}
            page={page}
            onPageChange={(value) => setSearch({ page: String(value) })}
          />
        </TabsContent>
        <TabsContent value="runs" className="mt-4">
          <WorkflowRuns
            status={search.get('status')}
            onStatusChange={(value) =>
              setSearch(
                value ? { view: 'runs', status: value } : { view: 'runs' }
              )
            }
          />
        </TabsContent>
      </Tabs>
    </PageContainer>
  )
}

function WorkflowDefinitions({
  query,
  page,
  onPageChange,
}: {
  query: UseQueryResult<WorkflowDefinitionsResponse>
  page: number
  onPageChange: (page: number) => void
}) {
  const rows = query.data?.workflows ?? []
  if (query.isError)
    return (
      <PageState
        variant="failed"
        size="compact"
        icon={Workflow}
        title="Could not load workflows"
        description={problemDetail(query.error, 'The server did not answer.')}
        action={
          <Button
            variant="outline"
            aria-disabled={query.isFetching}
            onClick={() => {
              if (!query.isFetching) void query.refetch()
            }}
          >
            {query.isFetching ? 'Trying again…' : 'Try again'}
          </Button>
        }
      />
    )
  if (!query.isPending && rows.length === 0)
    return (
      <WorkflowsSetupGuide
        title="No workflows yet"
        credentialAction={
          <Button asChild variant="outline">
            <Link to="/projects">Open a project’s Durable workflows tab</Link>
          </Button>
        }
      />
    )
  return (
    <DataTable<WorkflowDefinitionResponse>
      rows={rows}
      rowKey={(row) => `${row.environment_id}:${row.name}`}
      isLoading={query.isPending}
      columns={[
        {
          key: 'workflow',
          header: 'Workflow',
          render: (row) => (
            <div className="min-w-0">
              <RecordLink to={workflowHref(row)}>{row.name}</RecordLink>
              <p className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
                {row.project_slug} · {row.environment_slug}
                {row.version && ` · Version ${row.version}`}
              </p>
            </div>
          ),
        },
        {
          key: 'state',
          header: 'State',
          render: (row) => <Status {...definitionStatus(row.state)} />,
        },
        {
          key: 'active',
          header: 'Active',
          className: 'hidden @xl:table-cell',
          render: (row) => row.active_runs,
        },
        {
          key: 'failed',
          header: 'Failed',
          className: 'hidden @xl:table-cell',
          render: (row) => row.failed_runs,
        },
        {
          key: 'last',
          header: 'Last run',
          render: (row) =>
            row.last_run_at ? (
              <TimeAgo date={row.last_run_at} />
            ) : (
              <span className="text-muted-foreground">Never</span>
            ),
        },
      ]}
      pagination={
        (query.data?.total_pages ?? 1) > 1
          ? {
              page,
              pageSize: 20,
              total: query.data?.total ?? 0,
              totalPages: query.data?.total_pages ?? 1,
              onPageChange,
            }
          : undefined
      }
    />
  )
}

function WorkflowRuns({
  status: selected,
  onStatusChange,
}: {
  status: string | null
  onStatusChange: (status: WorkflowRunStatus | null) => void
}) {
  const status = workflowStatuses.includes(selected as WorkflowRunStatus)
    ? (selected as WorkflowRunStatus)
    : undefined
  const query = useInfiniteQuery({
    queryKey: ['all-workflow-runs', status],
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
  const failure = query.isError
    ? problemDetail(query.error, 'The server did not answer.')
    : null
  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center">
        <Select
          value={status ?? 'all'}
          onValueChange={(value) =>
            onStatusChange(
              value === 'all' ? null : (value as WorkflowRunStatus)
            )
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
        {/* With nothing loaded the failure state below says why; this
            line only covers a refresh failing over runs already shown. */}
        {failure && rows.length > 0 && (
          <p role="alert" className="text-sm text-destructive">
            Could not refresh workflow runs: {failure}
          </p>
        )}
      </div>
      {!query.isPending && rows.length === 0 ? (
        failure ? (
          <PageState
            variant="failed"
            size="compact"
            icon={History}
            title="Could not load workflow runs"
            description={`The server answered: ${failure}`}
            action={
              <Button
                variant="outline"
                aria-disabled={query.isFetching}
                onClick={() => {
                  if (!query.isFetching) void query.refetch()
                }}
              >
                {query.isFetching ? 'Trying again…' : 'Try again'}
              </Button>
            }
          />
        ) : (
          <PageState
            variant="empty"
            size="compact"
            icon={History}
            title={status ? 'No matching runs' : 'No workflow runs yet'}
            description={
              status
                ? `No run is ${status}.`
                : 'Runs appear here once an app starts a workflow.'
            }
            action={
              status ? (
                <Button variant="outline" onClick={() => onStatusChange(null)}>
                  Show all runs
                </Button>
              ) : (
                <Button asChild variant="outline">
                  <Link to="/workflows">See how to connect an app</Link>
                </Button>
              )
            }
          />
        )
      ) : (
        <DataTable<GlobalWorkflowRunResponse>
          rows={rows}
          rowKey={(run) => run.id}
          isLoading={query.isPending}
          columns={[
            {
              key: 'workflow',
              header: 'Run',
              render: (run) => (
                <div className="min-w-0">
                  <RecordLink to={runHref(run)}>{run.workflow}</RecordLink>
                  <p className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
                    {run.project_slug} · {run.environment_slug} ·{' '}
                    <span className="font-mono">{run.id.slice(0, 8)}</span>
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
              key: 'steps',
              header: 'Steps',
              className: 'hidden @xl:table-cell',
              render: (run) => run.step_count,
            },
            {
              key: 'created',
              header: 'Started',
              render: (run) => <TimeAgo date={run.created_at} />,
            },
          ]}
        />
      )}
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
    </div>
  )
}
