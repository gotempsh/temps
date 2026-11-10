// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useState } from 'react'
import { Link } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { Button } from '@temps-sdk/ui'
import { Callout, ResponsivePagination } from '@temps-sdk/ds'
import { projectAccessVaultsOptions } from '@/api/client/@tanstack/react-query.gen'

export function ProjectVaultAccess({ projectId }: { projectId: number }) {
  const [page, setPage] = useState(1)
  const vaults = useQuery(
    projectAccessVaultsOptions({
      path: { project_id: projectId },
      query: { page, page_size: 20 },
    })
  )
  return (
    <section className="space-y-3" aria-label="Linked vaults">
      <h2 className="font-semibold">Linked vaults</h2>
      {vaults.isError ? (
        <Callout tone="error" title="Could not load linked vaults">
          <Button onClick={() => void vaults.refetch()}>Retry</Button>
        </Callout>
      ) : vaults.isPending ? (
        <p role="status">Loading linked vaults…</p>
      ) : vaults.data?.total === 0 ? (
        <Callout title="No vaults linked">
          An administrator can link a vault to this project. Sandbox owners can
          then request a session from a bundle.
          <Button asChild variant="outline">
            <Link to="/vault">Open vaults</Link>
          </Button>
        </Callout>
      ) : (
        vaults.data?.items.map(({ vault }) => (
          <div key={vault.id} className="rounded-lg border p-3">
            {vault.name}
            <p className="text-sm text-muted-foreground">
              {vault.service_ids.length} services · Grant access from a sandbox
              or AI chat
            </p>
          </div>
        ))
      )}
      {(vaults.data?.total ?? 0) > 20 && (
        <ResponsivePagination
          page={page}
          pageSize={20}
          total={vaults.data?.total ?? 0}
          totalPages={Math.ceil((vaults.data?.total ?? 0) / 20)}
          onPageChange={setPage}
        />
      )}
    </section>
  )
}
