// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { useQuery } from '@tanstack/react-query'
import { DataTable, Detail, PageState, Status } from '@temps-sdk/ds'
import { Workflow } from 'lucide-react'
import { Link, useParams, useSearchParams } from 'react-router'
import {
  getProjectOptions,
  getEnvironmentOptions,
  getWorkflowRunOptions,
  listWorkflowStepsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { RefreshButton } from '@/components/ui/refresh-button'
import { Badge } from '@/components/ui/badge'
import { CopyButton } from '@/components/ui/copy-button'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { Skeleton } from '@/components/ui/skeleton'
import { usePageTitle } from '@/hooks/usePageTitle'
import { problemDetail } from '@/lib/api-problem'
import { workflowTone } from './status'

export function WorkflowRunDetail() {
  const params = useParams()
  const [search, setSearch] = useSearchParams()
  const page = Math.max(1, Number(search.get('page')) || 1)
  const path = {
    project_id: Number(params.projectId),
    environment_id: Number(params.environmentId),
    run_id: params.runId ?? '',
  }
  const runOptions = getWorkflowRunOptions({ path })
  const stepOptions = listWorkflowStepsOptions({
    path,
    query: { page, page_size: 20 },
  })
  const run = useQuery(runOptions)
  const steps = useQuery(stepOptions)
  const project = useQuery(getProjectOptions({ path: { id: path.project_id } }))
  const environment = useQuery(
    getEnvironmentOptions({
      path: { project_id: path.project_id, env_id: path.environment_id },
    })
  )
  const { setBreadcrumbs } = useBreadcrumbs()
  const workflowHref = `/workflows/${path.project_id}/${path.environment_id}?workflow=${encodeURIComponent(run.data?.workflow ?? '')}`
  const projectHref = project.data
    ? `/projects/${project.data.slug}`
    : '/projects'
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Workflows', href: '/workflows' },
      { label: project.data?.name ?? 'Project', href: projectHref },
      {
        label: environment.data?.name ?? 'Environment',
        href: `${projectHref}/durable-workflows?environment=${path.environment_id}`,
      },
      { label: run.data?.workflow ?? 'Workflow', href: workflowHref },
      { label: `Run ${path.run_id.slice(0, 8)}` },
    ])
    return () => setBreadcrumbs([])
  }, [
    setBreadcrumbs,
    project.data?.name,
    projectHref,
    environment.data?.name,
    path.environment_id,
    path.run_id,
    run.data?.workflow,
    workflowHref,
  ])
  usePageTitle(run.data ? `${run.data.workflow} · Run` : 'Workflow run')
  const refresh = (
    <RefreshButton
      queryKeys={[
        runOptions.queryKey,
        stepOptions.queryKey,
        getProjectOptions({ path: { id: path.project_id } }).queryKey,
        getEnvironmentOptions({
          path: { project_id: path.project_id, env_id: path.environment_id },
        }).queryKey,
      ]}
    />
  )
  if (run.isError)
    return (
      <PageState
        variant="failed"
        size="compact"
        icon={Workflow}
        title="Could not load this run"
        description={problemDetail(run.error, 'Refresh to try again.')}
        action={refresh}
      />
    )
  return (
    <Detail
      title={run.data?.workflow ?? 'Workflow run'}
      actions={refresh}
      verdict={
        <div className="flex basis-full flex-wrap items-center gap-x-3 gap-y-2 text-sm text-muted-foreground">
          {run.data ? (
            <Status
              tone={workflowTone(run.data.status)}
              label={run.data.status}
            />
          ) : (
            <Skeleton className="h-6 w-24" />
          )}
          {project.data ? (
            <Link
              className="inline-flex items-center gap-1.5 hover:underline"
              to={projectHref}
            >
              <ServiceIcon service={project.data} size="xxs" />
              {project.data.name}
            </Link>
          ) : (
            <span>
              {project.isError ? 'Project unavailable' : 'Loading project…'}
            </span>
          )}
          <Badge variant="secondary">
            {environment.data?.name ??
              (environment.isError
                ? 'Environment unavailable'
                : 'Loading environment…')}
          </Badge>
          {run.data && (
            <>
              <span>
                Started <TimeAgo date={run.data.created_at} />
              </span>
              <span>Attempt {run.data.attempt}</span>
              <span>{run.data.step_count} steps</span>
            </>
          )}
        </div>
      }
      facts={[]}
      main={
        <div className="space-y-4">
          <div className="flex flex-wrap items-center justify-between gap-3 border-b pb-4">
            <div className="flex min-w-0 flex-wrap items-center gap-2">
              <h2 className="font-medium">Execution</h2>
              <span className="text-xs text-muted-foreground">
                Run {path.run_id.slice(0, 8)}
              </span>
              <CopyButton value={path.run_id} label="Copy run ID" minimal />
            </div>
            <Link
              to={workflowHref}
              className="text-sm underline underline-offset-4"
            >
              View workflow runs
            </Link>
          </div>
          {run.data?.blocked_reason && (
            <p role="status" className="text-sm">
              {run.data.blocked_reason}
            </p>
          )}
          {run.data?.error_kind && (
            <p className="text-sm text-destructive">{run.data.error_kind}</p>
          )}
          {steps.isError ? (
            <p role="alert">
              {problemDetail(
                steps.error,
                'Could not load the journal. Refresh to retry.'
              )}
            </p>
          ) : (
            <DataTable
              aria-label="Workflow journal"
              rows={steps.data?.steps ?? []}
              rowKey={(step) => step.sequence}
              isLoading={steps.isPending}
              columns={[
                {
                  key: 'name',
                  header: 'Step',
                  render: (step) => (
                    <span className="[overflow-wrap:anywhere]">
                      {step.sequence + 1}. {step.name}
                      <span className="mt-1 block text-xs text-muted-foreground @xl:hidden">
                        {step.kind} · {step.attempts} attempts
                      </span>
                    </span>
                  ),
                },
                {
                  key: 'kind',
                  header: 'Kind',
                  className: 'hidden @xl:table-cell',
                  render: (step) => step.kind,
                },
                {
                  key: 'status',
                  header: 'State',
                  render: (step) => (
                    <Status
                      tone={workflowTone(step.status)}
                      label={step.status}
                    />
                  ),
                },
                {
                  key: 'attempts',
                  header: 'Attempts',
                  className: 'hidden @xl:table-cell',
                  render: (step) => step.attempts,
                },
              ]}
              pagination={
                (steps.data?.total_pages ?? 1) > 1
                  ? {
                      page,
                      pageSize: 20,
                      total: steps.data?.total ?? 0,
                      totalPages: Math.max(1, steps.data?.total_pages ?? 1),
                      onPageChange: (value) =>
                        setSearch({ page: String(value) }),
                    }
                  : undefined
              }
            />
          )}
        </div>
      }
    />
  )
}
