// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Button, Skeleton } from '@temps-sdk/ui'
import { Callout, ResponsivePagination } from '@temps-sdk/ds'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import {
  getProjectsOptions,
  listSandboxesOptions,
} from '@/api/client/@tanstack/react-query.gen'

export function VaultConsumerPicker({
  kind,
  selected,
  onSelect,
}: {
  kind: 'project' | 'sandbox'
  selected?: number | string
  onSelect: (id: number | string) => void
}) {
  const [page, setPage] = useState(1)
  const projects = useQuery({
    ...getProjectsOptions({ query: { page, per_page: 20 } }),
    enabled: kind === 'project',
  })
  const sandboxes = useQuery({
    ...listSandboxesOptions({ query: { page, page_size: 20 } }),
    enabled: kind === 'sandbox',
  })
  const query = kind === 'project' ? projects : sandboxes
  const rows =
    kind === 'project'
      ? projects.data?.projects.map((project) => ({
          ...project,
          icon: <ServiceIcon service={project} size="xs" />,
        }))
      : sandboxes.data?.sandboxes.map((sandbox) => ({ ...sandbox, icon: null }))
  const total =
    kind === 'project' ? projects.data?.total : sandboxes.data?.pagination.count
  return (
    <div className="space-y-3">
      {query.isError ? (
        <Callout tone="error" title={`Could not load ${kind}s`}>
          <Button type="button" onClick={() => void query.refetch()}>
            Retry
          </Button>
        </Callout>
      ) : query.isPending ? (
        <div role="status" aria-label="Loading consumers" className="space-y-3">
          {[0, 1, 2].map((row) => (
            <div key={row}>
              <Skeleton className="h-9 w-32 rounded-md" />
            </div>
          ))}
        </div>
      ) : !rows?.length ? (
        <p>No {kind}s on this page.</p>
      ) : (
        rows.map((row) => (
          <Button
            key={row.id}
            type="button"
            variant={selected === row.id ? 'default' : 'outline'}
            aria-pressed={selected === row.id}
            onClick={() => onSelect(row.id)}
          >
            {row.icon}
            {row.name}
          </Button>
        ))
      )}
      {(total ?? 0) > 20 && (
        <ResponsivePagination
          page={page}
          pageSize={20}
          total={total ?? 0}
          totalPages={Math.ceil((total ?? 0) / 20)}
          onPageChange={setPage}
        />
      )}
    </div>
  )
}
