// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { EchoDialog, Ledger, PageState, Status } from '@temps-sdk/ds'
import { KeyRound } from 'lucide-react'
import { Link, useParams, useSearchParams } from 'react-router'
import { toast } from 'sonner'
import { listWorkflowProjectKeysOptions } from '@/api/client/@tanstack/react-query.gen'
import { shredWorkflowProjectKey } from '@/api/client/sdk.gen'
import type { WorkflowProjectKeyResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import { RefreshButton } from '@/components/ui/refresh-button'
import { usePageTitle } from '@/hooks/usePageTitle'
import { problemDetail } from '@/lib/api-problem'

export function ProjectWorkflowKeys() {
  const { projectId } = useParams()
  const project_id = Number(projectId)
  const [search, setSearch] = useSearchParams()
  const page = Math.max(1, Number(search.get('page')) || 1)
  const [selected, setSelected] = useState<WorkflowProjectKeyResponse | null>(
    null
  )
  const client = useQueryClient()
  const options = listWorkflowProjectKeysOptions({
    path: { project_id },
    query: { page, page_size: 20 },
  })
  const query = useQuery(options)
  usePageTitle('Workflow encryption keys')
  const mutation = useMutation({
    mutationFn: async (key: WorkflowProjectKeyResponse) => {
      await shredWorkflowProjectKey({
        path: { project_id, key_id: key.id },
        body: { confirm_key_id: key.id },
        throwOnError: true,
      })
    },
    onSuccess: async () => {
      setSelected(null)
      toast.success('Workflow key shredded')
      await client.invalidateQueries({ queryKey: options.queryKey })
    },
    onError: (error) =>
      toast.error(problemDetail(error, 'Could not shred the workflow key.')),
  })
  return (
    <>
      <Ledger<WorkflowProjectKeyResponse>
        title="Workflow encryption keys"
        actions={
          <div className="flex flex-wrap gap-2">
            <Button asChild variant="outline">
              <Link to="/workflows">All workflows</Link>
            </Button>
            <RefreshButton queryKeys={[options.queryKey]} />
          </div>
        }
        toolbar={
          <p className="text-sm text-muted-foreground">
            Project {project_id}. Shredding permanently removes access to
            payloads using that key. Backups containing the key can restore
            access. Legacy payloads use the instance key.
          </p>
        }
        rows={query.data?.keys ?? []}
        rowKey={(key) => key.id}
        columns={[
          {
            key: 'id',
            header: 'Key',
            render: (key) => (
              <span className="font-mono text-xs [overflow-wrap:anywhere]">
                {key.id}
              </span>
            ),
          },
          {
            key: 'state',
            header: 'State',
            render: (key) => (
              <Status
                tone={key.shredded_at ? 'idle' : 'ok'}
                label={key.shredded_at ? 'Shredded' : 'Active'}
              />
            ),
          },
          {
            key: 'created',
            header: 'Created',
            render: (key) => new Date(key.created_at).toLocaleString(),
          },
          {
            key: 'actions',
            header: '',
            render: (key) =>
              !key.shredded_at && (
                <Button
                  variant="destructive"
                  size="sm"
                  onClick={() => setSelected(key)}
                >
                  Shred key
                </Button>
              ),
          },
        ]}
        isLoading={query.isPending}
        pagination={
          query.data
            ? {
                page,
                totalPages: query.data.total_pages,
                total: query.data.total,
                pageSize: 20,
                onPageChange: (value) => setSearch({ page: String(value) }),
              }
            : undefined
        }
        empty={
          <PageState
            variant={query.isError ? 'failed' : 'empty'}
            size="compact"
            icon={KeyRound}
            title={query.isError ? 'Could not load keys' : 'No project key yet'}
            description={
              query.isError
                ? problemDetail(query.error, 'Refresh to retry.')
                : 'The first new workflow payload creates a project key automatically.'
            }
          />
        }
      />
      <EchoDialog
        open={selected !== null}
        onOpenChange={(open) => {
          if (!open && !mutation.isPending) setSelected(null)
        }}
        title="Permanently shred this workflow key?"
        description="Existing runs using this key will no longer be able to read their inputs, results, or errors. This also affects sleeping and waiting runs. New payloads receive a new key. Backups may retain the old key."
        phrase={selected?.id ?? ''}
        confirmLabel="Shred key"
        busy={mutation.isPending}
        onConfirm={() => {
          if (selected && !mutation.isPending) mutation.mutate(selected)
        }}
      />
    </>
  )
}
