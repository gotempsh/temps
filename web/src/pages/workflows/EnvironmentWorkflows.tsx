// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Ledger, PageState, Status, Field, EchoDialog } from '@temps-sdk/ds'
import { Workflow } from 'lucide-react'
import { Link, useNavigate } from 'react-router'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { z } from 'zod'
import { zodResolver } from '@hookform/resolvers/zod'
import { toast } from 'sonner'
import type { ProjectResponse, WorkflowSummaryResponse } from '@/api/client'
import { getWorkflowsOverviewOptions } from '@/api/client/@tanstack/react-query.gen'
import { startWorkflowRun, issueWorkflowCredential } from '@/api/client/sdk.gen'
import { useProjectEnvironment } from '@/components/project-frame/project-environment-context'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'
import { RefreshButton } from '@/components/ui/refresh-button'
import { problemDetail } from '@/lib/api-problem'
import { runHref } from './status'

const schema = z.object({
  workflow: z
    .string()
    .min(1)
    .max(128)
    .regex(/^[a-zA-Z0-9._:/-]+$/),
  input: z.string().refine((value) => {
    try {
      const parsed = JSON.parse(value)
      return (
        parsed !== null && typeof parsed === 'object' && !Array.isArray(parsed)
      )
    } catch {
      return false
    }
  }, 'Enter a JSON object, for example {"orderId":1}'),
})
type Fields = z.infer<typeof schema>

export function EnvironmentWorkflows({
  project,
}: {
  project: ProjectResponse
}) {
  const bar = useProjectEnvironment()
  const environment = bar.selection.environment
  if (!environment)
    return (
      <PageState
        variant={bar.isError ? 'failed' : 'empty'}
        size="compact"
        icon={Workflow}
        title="Choose an environment"
        description="Select an environment to configure and run durable workflows."
      />
    )
  return (
    <WorkflowEnvironment
      key={environment.id}
      projectId={project.id}
      environmentId={environment.id}
    />
  )
}

export function WorkflowEnvironment({
  projectId,
  environmentId,
}: {
  projectId: number
  environmentId: number
}) {
  const path = { project_id: projectId, environment_id: environmentId }
  const options = getWorkflowsOverviewOptions({ path })
  const query = useQuery(options)
  const cache = useQueryClient()
  const navigate = useNavigate()
  const [credential, setCredential] = useState<string | null>(null)
  const [rotating, setRotating] = useState(false)
  const form = useForm<Fields>({
    resolver: zodResolver(schema),
    defaultValues: { workflow: '', input: '{}' },
  })
  const start = useMutation({
    mutationFn: async (value: Fields) =>
      (
        await startWorkflowRun({
          path,
          body: {
            workflow: value.workflow,
            input: JSON.parse(value.input),
            idempotency_key: crypto.randomUUID(),
          },
          throwOnError: true,
        })
      ).data,
    onSuccess: (result) => {
      toast.success('Workflow queued')
      navigate(runHref(result.run))
    },
    onError: (error) =>
      toast.error(problemDetail(error, 'Could not start the workflow.')),
  })
  const issue = useMutation({
    mutationFn: async () =>
      (await issueWorkflowCredential({ path, throwOnError: true })).data,
    onSuccess: async (result) => {
      setCredential(result.token)
      setRotating(false)
      toast.success('Workflow credential created')
      await cache.invalidateQueries({ queryKey: options.queryKey })
    },
    onError: (error) =>
      toast.error(problemDetail(error, 'Could not create the credential.')),
  })
  return (
    <>
      <Ledger<WorkflowSummaryResponse>
        title="Durable workflows"
        rows={query.data?.workflows ?? []}
        rowKey={(row) => row.name}
        isLoading={query.isPending}
        actions={
          <div className="flex flex-wrap gap-2">
            <Button asChild variant="outline">
              <Link to={`/workflows/${projectId}/${environmentId}/settings`}>
                Settings
              </Link>
            </Button>
            <Button asChild variant="outline">
              <Link to={`/workflows/${projectId}/${environmentId}/grants`}>
                Access grants
              </Link>
            </Button>
            <Button asChild variant="outline">
              <Link to={`/workflows/${projectId}/keys`}>Encryption keys</Link>
            </Button>
            <Button asChild variant="outline">
              <Link to="/workflows">All runs</Link>
            </Button>
            <RefreshButton queryKeys={[options.queryKey]} />
          </div>
        }
        toolbar={
          <div className="space-y-4">
            <details className="rounded-lg border p-4">
              <summary className="cursor-pointer text-sm font-medium">
                Local development and deployment triggers
              </summary>
              <div className="mt-3 space-y-3 text-sm">
                <p>
                  For an undeployed, unprotected preview environment, run your
                  SDK server locally with this environment’s workflow
                  credential, then connect it:
                </p>
                <code className="block [overflow-wrap:anywhere]">{`temps workflows dev --project-id ${projectId} --environment-id ${environmentId} --port 3000`}</code>
                <p>
                  To start a workflow after each successful source deployment,
                  add <code>deploymentWorkflows: [after-deploy]</code> to{' '}
                  <code>.temps.yaml</code>. The deployment job logs the run ID;
                  retrying that job reuses the run.
                </p>
              </div>
            </details>
            {query.data && !query.data.executor.configured && (
              <PageState
                variant="not-set-up"
                size="compact"
                icon={Workflow}
                title="Executor not ready"
                requirement={
                  query.data.executor.reason ??
                  'Deploy an app using a Temps workflows SDK.'
                }
                example="An orders workflow can charge a payment, sleep, then wait for approval."
                settingsHref={
                  query.data.executor.setup_path ??
                  `/workflows/${projectId}/${environmentId}/settings`
                }
              />
            )}
            <div className="flex flex-wrap items-center gap-2">
              <Status
                tone={query.data?.credential.configured ? 'ok' : 'warn'}
                label={
                  query.data?.credential.configured
                    ? 'Credential configured'
                    : 'No workflow credential'
                }
              />
              <Button
                variant="outline"
                aria-disabled={issue.isPending}
                onClick={() => {
                  if (!issue.isPending) {
                    if (query.data?.credential.configured) setRotating(true)
                    else issue.mutate()
                  }
                }}
              >
                {query.data?.credential.configured
                  ? 'Rotate credential'
                  : 'Create credential'}
              </Button>
            </div>
            {credential && (
              <div role="status" className="space-y-2 rounded-lg border p-4">
                <p>
                  Copy this credential now. Set it as TEMPS_WORKFLOWS_TOKEN in
                  your app; it will not be shown again.
                </p>
                <code className="block break-all">{credential}</code>
                <CopyButton value={credential} />
                <Button variant="outline" onClick={() => setCredential(null)}>
                  Dismiss credential
                </Button>
              </div>
            )}
            <form
              className="space-y-3 rounded-lg border p-4"
              onSubmit={form.handleSubmit((value) => {
                if (!start.isPending) start.mutate(value)
              })}
            >
              <Field
                label="Workflow name"
                error={form.formState.errors.workflow?.message}
              >
                {(props) => (
                  <Input
                    {...props}
                    {...form.register('workflow')}
                    placeholder="orders"
                  />
                )}
              </Field>
              <Field
                label="Input JSON"
                error={form.formState.errors.input?.message}
              >
                {(props) => <Textarea {...props} {...form.register('input')} />}
              </Field>
              <Button type="submit" aria-disabled={start.isPending}>
                {start.isPending ? 'Starting…' : 'Start run'}
              </Button>
              {!query.data?.executor.configured && (
                <p className="text-sm text-muted-foreground">
                  Runs stay queued until an executor is ready.
                </p>
              )}
            </form>
          </div>
        }
        columns={[
          {
            key: 'name',
            header: 'Workflow',
            render: (row) => (
              <span className="[overflow-wrap:anywhere]">{row.name}</span>
            ),
          },
          {
            key: 'defined',
            header: 'Executor',
            render: (row) => (
              <Status
                tone={row.defined ? 'ok' : 'warn'}
                label={row.defined ? 'Available' : 'Not in current deployment'}
              />
            ),
          },
          {
            key: 'active',
            header: 'Active runs',
            render: (row) => row.active_runs,
          },
        ]}
        empty={
          <PageState
            variant={query.isError ? 'failed' : 'empty'}
            size="compact"
            icon={Workflow}
            title={
              query.isError
                ? 'Could not load workflows'
                : 'No workflows detected'
            }
            description={
              query.isError
                ? problemDetail(query.error, 'Refresh to retry.')
                : 'Define an orders workflow in your app, deploy it, then start a run here.'
            }
          />
        }
      />
      <EchoDialog
        open={rotating}
        onOpenChange={setRotating}
        title="Rotate workflow credential?"
        description="The old credential stops working immediately. Update your app with the new credential."
        phrase="rotate"
        confirmLabel="Rotate credential"
        busy={issue.isPending}
        onConfirm={() => {
          if (!issue.isPending) issue.mutate()
        }}
      />
    </>
  )
}
