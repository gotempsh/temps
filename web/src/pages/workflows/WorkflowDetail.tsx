// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Ledger, Status } from '@temps-sdk/ds'
import { Link, useParams, useSearchParams } from 'react-router'
import {
  listWorkflowRunsOptions,
  getProjectOptions,
  getEnvironmentOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { RefreshButton } from '@/components/ui/refresh-button'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { usePageTitle } from '@/hooks/usePageTitle'
import { problemDetail } from '@/lib/api-problem'
import { runHref, workflowTone } from './status'

export function WorkflowDetail() {
  const params = useParams()
  const [search, setSearch] = useSearchParams()
  const workflow = search.get('workflow') ?? ''
  usePageTitle(workflow || 'Workflow runs')
  const page = Math.max(1, Number(search.get('page')) || 1)
  const path = {
    project_id: Number(params.projectId),
    environment_id: Number(params.environmentId),
  }
  const options = listWorkflowRunsOptions({
    path,
    query: { workflow, page, page_size: 20 },
  })
  const runs = useQuery(options)
  const project = useQuery(getProjectOptions({ path: { id: path.project_id } }))
  const environment = useQuery(
    getEnvironmentOptions({
      path: { project_id: path.project_id, env_id: path.environment_id },
    })
  )
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Workflows', href: '/workflows' },
      {
        label: project.data?.name ?? 'Project',
        href: project.data ? `/projects/${project.data.slug}` : '/projects',
      },
      {
        label: environment.data?.name ?? 'Environment',
        href: project.data
          ? `/projects/${project.data.slug}/durable-workflows?environment=${path.environment_id}`
          : '/projects',
      },
      { label: workflow || 'Workflow' },
    ])
    return () => setBreadcrumbs([])
  }, [
    setBreadcrumbs,
    workflow,
    project.data,
    environment.data?.name,
    path.environment_id,
  ])
  return (
    <Ledger
      title={workflow || 'Workflow runs'}
      actions={<RefreshButton queryKeys={[options.queryKey]} />}
      toolbar={
        <div className="flex flex-wrap gap-4 text-sm">
          <Link
            className="underline underline-offset-4"
            to={`/workflows/${path.project_id}/${path.environment_id}/settings`}
          >
            Workflow settings
          </Link>
          <Link
            className="underline underline-offset-4"
            to={`/workflows/${path.project_id}/${path.environment_id}/grants`}
          >
            Access grants
          </Link>
          <Link
            className="underline underline-offset-4"
            to={`/workflows/${path.project_id}/keys`}
          >
            Encryption keys
          </Link>
          {runs.isError && (
            <p role="alert">
              {problemDetail(
                runs.error,
                'Could not load runs. Refresh to retry.'
              )}
            </p>
          )}
        </div>
      }
      rows={runs.data?.runs ?? []}
      rowKey={(run) => run.id}
      isLoading={runs.isPending}
      columns={[
        {
          key: 'run',
          header: 'Run',
          render: (run) => (
            <Link className="underline underline-offset-4" to={runHref(run)}>
              {run.id.slice(0, 8)}
            </Link>
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
      pagination={{
        page,
        pageSize: 20,
        total: runs.data?.total ?? 0,
        totalPages: Math.max(1, runs.data?.total_pages ?? 1),
        onPageChange: (value) => setSearch({ workflow, page: String(value) }),
      }}
    />
  )
}
