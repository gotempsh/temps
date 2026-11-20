// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { type UseQueryResult, useQuery } from '@tanstack/react-query'
import {
  DataTable,
  Detail,
  type DetailFact,
  PageState,
  RecordLink,
  Status,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from '@temps-sdk/ds'
import { History, KeyRound, Settings2, ShieldCheck } from 'lucide-react'
import { useParams, useSearchParams } from 'react-router'
import type {
  ListWorkflowRunsResponse,
  WorkflowRunResponse,
  WorkflowSummaryResponse,
} from '@/api/client'

type WorkflowRunsPage = ListWorkflowRunsResponse
import {
  listWorkflowRunsOptions,
  getProjectOptions,
  getEnvironmentOptions,
  getWorkflowsOverviewOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { RefreshButton } from '@/components/ui/refresh-button'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { usePageTitle } from '@/hooks/usePageTitle'
import { problemDetail } from '@/lib/api-problem'
import { runHref, workflowTone } from './status'
import { WorkflowGrantsPanel } from './WorkflowGrants'
import { WorkflowKeysPanel } from './ProjectWorkflowKeys'
import { WorkflowSettingsPanel } from './WorkflowSettings'
import {
  workflowDetailCrumbs,
  workflowDetailTab,
  type WorkflowDetailTab,
} from './detail-tabs'

export function WorkflowDetail() {
  const params = useParams()
  const [search, setSearch] = useSearchParams()
  const workflow = search.get('workflow') ?? ''
  const tab = workflowDetailTab(search.get('tab'))
  usePageTitle(workflow || 'Workflow runs')
  const page = Math.max(1, Number(search.get('page')) || 1)
  const path = {
    project_id: Number(params.projectId),
    environment_id: Number(params.environmentId),
  }
  // Every tab keeps the workflow in the URL; only the open tab pages.
  const go = (next: WorkflowDetailTab, nextPage?: number) =>
    setSearch({
      workflow,
      ...(next === 'runs' ? {} : { tab: next }),
      ...(nextPage && nextPage > 1 ? { page: String(nextPage) } : {}),
    })
  const runsOptions = listWorkflowRunsOptions({
    path,
    query: { workflow, page: tab === 'runs' ? page : 1, page_size: 20 },
  })
  const runs = useQuery(runsOptions)
  const overviewOptions = getWorkflowsOverviewOptions({ path })
  const overview = useQuery(overviewOptions)
  const summary = overview.data?.workflows.find((row) => row.name === workflow)
  const project = useQuery(getProjectOptions({ path: { id: path.project_id } }))
  const environment = useQuery(
    getEnvironmentOptions({
      path: { project_id: path.project_id, env_id: path.environment_id },
    })
  )
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => {
    setBreadcrumbs(
      workflowDetailCrumbs({
        projectId: path.project_id,
        environmentId: path.environment_id,
        workflow,
        tab,
        project: project.data,
        environment: environment.data,
      })
    )
    return () => setBreadcrumbs([])
  }, [
    setBreadcrumbs,
    workflow,
    tab,
    project.data,
    environment.data,
    path.project_id,
    path.environment_id,
  ])
  const environmentName = environment.data?.slug ?? 'this environment'
  return (
    <Detail
      title={workflow || 'Workflow runs'}
      description={
        project.data && environment.data
          ? `${project.data.slug} · ${environment.data.slug}`
          : undefined
      }
      verdict={summary && <DefinitionVerdict summary={summary} />}
      actions={
        <RefreshButton
          queryKeys={[runsOptions.queryKey, overviewOptions.queryKey]}
        />
      }
      facts={summary ? workflowFacts(summary) : []}
      main={
        <Tabs
          value={tab}
          onValueChange={(value) => go(workflowDetailTab(value))}
        >
          <TabsList>
            <TabsTrigger value="runs" icon={History} count={runs.data?.total}>
              Runs
            </TabsTrigger>
            <TabsTrigger value="settings" icon={Settings2}>
              Settings
            </TabsTrigger>
            <TabsTrigger value="access" icon={ShieldCheck}>
              Access
            </TabsTrigger>
            <TabsTrigger value="keys" icon={KeyRound}>
              Encryption keys
            </TabsTrigger>
          </TabsList>
          <TabsContent value="runs" className="mt-4">
            <WorkflowRuns
              workflow={workflow}
              query={runs}
              page={page}
              onPageChange={(value) => go('runs', value)}
            />
          </TabsContent>
          <TabsContent value="settings" className="mt-4">
            <WorkflowSettingsPanel
              path={path}
              embedded
              description={`Applies to every workflow in ${environmentName}.`}
            />
          </TabsContent>
          <TabsContent value="access" className="mt-4">
            <WorkflowGrantsPanel
              path={path}
              page={page}
              onPageChange={(value) => go('access', value)}
              description={`Other environments allowed to start workflows or send events in ${environmentName}.`}
            />
          </TabsContent>
          <TabsContent value="keys" className="mt-4">
            <WorkflowKeysPanel
              projectId={path.project_id}
              page={page}
              onPageChange={(value) => go('keys', value)}
            />
          </TabsContent>
        </Tabs>
      }
    />
  )
}

function DefinitionVerdict({ summary }: { summary: WorkflowSummaryResponse }) {
  return summary.defined ? (
    <Status tone="ok" label="Deployed" />
  ) : (
    <Status tone="warn" label="Not in deployment" />
  )
}

/** The workflow's version, run counts and last run, for the record header. */
function workflowFacts(summary: WorkflowSummaryResponse): DetailFact[] {
  return [
    { label: 'Version', value: summary.version ?? '—' },
    { label: 'Active', value: summary.active_runs },
    { label: 'Completed', value: summary.completed_runs },
    { label: 'Failed', value: summary.failed_runs },
    {
      label: 'Concurrency',
      value: summary.concurrency ?? 'Unlimited',
    },
    {
      label: 'Last run',
      value: summary.last_run_at ? (
        <TimeAgo date={summary.last_run_at} />
      ) : (
        'Never'
      ),
    },
  ]
}

function WorkflowRuns({
  workflow,
  query,
  page,
  onPageChange,
}: {
  workflow: string
  query: UseQueryResult<WorkflowRunsPage, unknown>
  page: number
  onPageChange: (page: number) => void
}) {
  const rows = query.data?.runs ?? []
  if (!query.isPending && rows.length === 0)
    return (
      <PageState
        variant={query.isError ? 'failed' : 'empty'}
        size="compact"
        icon={History}
        title={query.isError ? 'Could not load runs' : 'No runs yet'}
        description={
          query.isError
            ? problemDetail(query.error, 'The server did not answer.')
            : `Start ${workflow || 'this workflow'} from your app or the environment's Durable workflows tab.`
        }
      />
    )
  return (
    <div className="space-y-3">
      {query.isError && (
        <p role="alert" className="text-sm text-destructive">
          {problemDetail(query.error, 'Could not refresh runs.')}
        </p>
      )}
      <DataTable<WorkflowRunResponse>
        aria-label={`Runs of ${workflow || 'this workflow'}`}
        rows={rows}
        rowKey={(run) => run.id}
        isLoading={query.isPending}
        columns={[
          {
            key: 'run',
            header: 'Run',
            render: (run) => (
              <RecordLink to={runHref(run)} className="font-mono">
                {run.id.slice(0, 8)}
              </RecordLink>
            ),
          },
          {
            key: 'state',
            header: 'State',
            render: (run) => (
              <Status tone={workflowTone(run.status)} label={run.status} />
            ),
          },
          {
            key: 'started',
            header: 'Started',
            render: (run) => <TimeAgo date={run.created_at} />,
          },
          {
            key: 'steps',
            header: 'Steps',
            className: 'hidden @xl:table-cell',
            render: (run) => run.step_count,
          },
        ]}
        pagination={
          (query.data?.total_pages ?? 1) <= 1
            ? undefined
            : {
                page,
                pageSize: 20,
                total: query.data?.total ?? 0,
                totalPages: Math.max(1, query.data?.total_pages ?? 1),
                onPageChange,
              }
        }
      />
    </div>
  )
}
