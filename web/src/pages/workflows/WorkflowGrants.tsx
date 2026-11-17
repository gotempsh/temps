// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { z } from 'zod'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Ledger, PageState } from '@temps-sdk/ds'
import { ShieldCheck } from 'lucide-react'
import { Link, useParams, useSearchParams } from 'react-router'
import { toast } from 'sonner'
import { listWorkflowGrantsOptions } from '@/api/client/@tanstack/react-query.gen'
import { createWorkflowGrant, revokeWorkflowGrant } from '@/api/client/sdk.gen'
import type { WorkflowGrantResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { RefreshButton } from '@/components/ui/refresh-button'
import { usePageTitle } from '@/hooks/usePageTitle'
import { problemDetail } from '@/lib/api-problem'

const schema = z
  .object({
    source_environment_id: z
      .string()
      .regex(/^[1-9]\d*$/, 'Enter a source environment ID'),
    action: z.enum(['start', 'event']),
    resource: z
      .string()
      .min(1)
      .max(200)
      .regex(
        /^[a-zA-Z0-9._:/-]+$/,
        'Use letters, digits, dots, underscores, colons, slashes or hyphens'
      ),
  })
  .refine((value) => value.action !== 'start' || value.resource.length <= 128, {
    path: ['resource'],
    message: 'Workflow names have at most 128 characters',
  })
type Fields = z.infer<typeof schema>

export function WorkflowGrants() {
  const params = useParams()
  const path = {
    project_id: Number(params.projectId),
    environment_id: Number(params.environmentId),
  }
  const [search, setSearch] = useSearchParams()
  const page = Math.max(1, Number(search.get('page')) || 1)
  const [selected, setSelected] = useState<WorkflowGrantResponse | null>(null)
  const client = useQueryClient()
  const options = listWorkflowGrantsOptions({
    path,
    query: { page, page_size: 20 },
  })
  const query = useQuery(options)
  const form = useForm<Fields>({
    resolver: zodResolver(schema),
    defaultValues: { source_environment_id: '', action: 'start', resource: '' },
  })
  usePageTitle('Workflow access grants')
  const invalidate = () =>
    client.invalidateQueries({ queryKey: options.queryKey })
  const create = useMutation({
    mutationFn: async (value: Fields) => {
      await createWorkflowGrant({
        path,
        body: {
          ...value,
          source_environment_id: Number(value.source_environment_id),
        },
        throwOnError: true,
      })
    },
    onSuccess: async () => {
      form.reset()
      toast.success('Workflow access granted')
      await invalidate()
    },
    onError: (error) =>
      toast.error(problemDetail(error, 'Could not create the grant.')),
  })
  const revoke = useMutation({
    mutationFn: async (grant: WorkflowGrantResponse) => {
      await revokeWorkflowGrant({
        path: { ...path, grant_id: grant.id },
        throwOnError: true,
      })
    },
    onSuccess: async () => {
      setSelected(null)
      toast.success('Workflow access revoked')
      await invalidate()
    },
    onError: (error) =>
      toast.error(problemDetail(error, 'Could not revoke the grant.')),
  })
  return (
    <>
      <Ledger<WorkflowGrantResponse>
        title="Workflow access grants"
        actions={
          <div className="flex flex-wrap gap-2">
            <Button asChild variant="outline">
              <Link to="/workflows">All workflows</Link>
            </Button>
            <RefreshButton queryKeys={[options.queryKey]} />
          </div>
        }
        toolbar={
          <form
            onSubmit={form.handleSubmit((value) => {
              if (!create.isPending) create.mutate(value)
            })}
            className="grid grid-cols-1 gap-3 rounded-lg border p-4 md:grid-cols-4"
          >
            <div className="space-y-1">
              <Label htmlFor="workflow-source">Source environment ID</Label>
              <Input
                id="workflow-source"
                inputMode="numeric"
                {...form.register('source_environment_id')}
              />
              <p className="text-sm text-destructive">
                {form.formState.errors.source_environment_id?.message}
              </p>
            </div>
            <fieldset className="space-y-1">
              <legend className="text-sm font-medium">Allowed operation</legend>
              <label className="flex items-center gap-2 text-sm">
                <input
                  type="radio"
                  value="start"
                  {...form.register('action')}
                />
                Start workflow
              </label>
              <label className="flex items-center gap-2 text-sm">
                <input
                  type="radio"
                  value="event"
                  {...form.register('action')}
                />
                Send event
              </label>
            </fieldset>
            <div className="space-y-1">
              <Label htmlFor="workflow-resource">
                Exact workflow or event name
              </Label>
              <Input id="workflow-resource" {...form.register('resource')} />
              <p className="text-sm text-destructive">
                {form.formState.errors.resource?.message}
              </p>
            </div>
            <div className="flex items-end">
              <Button type="submit" aria-disabled={create.isPending}>
                {create.isPending ? 'Granting…' : 'Grant access'}
              </Button>
            </div>
            <p className="text-sm text-muted-foreground md:col-span-4">
              Destination: project {path.project_id}, environment{' '}
              {path.environment_id}. Grants allow only the named operation. Run
              results stay private to this environment.
            </p>
          </form>
        }
        rows={query.data?.grants ?? []}
        rowKey={(grant) => grant.id}
        isLoading={query.isPending}
        columns={[
          {
            key: 'resource',
            header: 'Resource',
            render: (grant) => (
              <span className="[overflow-wrap:anywhere]">{grant.resource}</span>
            ),
          },
          {
            key: 'source',
            header: 'Source environment',
            render: (grant) => grant.source_environment_id,
          },
          {
            key: 'action',
            header: 'Permission',
            render: (grant) =>
              grant.action === 'start' ? 'Start workflow' : 'Send event',
          },
          {
            key: 'revoke',
            header: '',
            render: (grant) => (
              <Button
                variant="outline"
                size="sm"
                onClick={() => setSelected(grant)}
              >
                Revoke
              </Button>
            ),
          },
        ]}
        pagination={
          query.data
            ? {
                page,
                pageSize: 20,
                total: query.data.total,
                totalPages: query.data.total_pages,
                onPageChange: (value) => setSearch({ page: String(value) }),
              }
            : undefined
        }
        empty={
          <PageState
            variant={query.isError ? 'failed' : 'empty'}
            size="compact"
            icon={ShieldCheck}
            title={
              query.isError
                ? 'Could not load grants'
                : 'No external workflow access'
            }
            description={
              query.isError
                ? problemDetail(query.error, 'Refresh to retry.')
                : 'Grant a source environment permission to start one workflow or send one event here.'
            }
          />
        }
      />
      <ConfirmDeleteDialog
        open={selected !== null}
        onOpenChange={(open) => {
          if (!open && !revoke.isPending) setSelected(null)
        }}
        resourceType="workflow access grant"
        resourceName={selected?.resource ?? ''}
        verb="Revoke"
        requireTypedName
        consequences="New calls from the source environment will be rejected. Already authorized requests and runs may finish."
        confirmLabel="Revoke access"
        isPending={revoke.isPending}
        onConfirm={() => {
          if (selected && !revoke.isPending) revoke.mutate(selected)
        }}
      />
    </>
  )
}
