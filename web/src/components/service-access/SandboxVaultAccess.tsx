// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useState } from 'react'
import { Link } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { Button } from '@temps-sdk/ui'
import { Callout, ResponsivePagination } from '@temps-sdk/ds'
import { sandboxAccessVaultsOptions } from '@/api/client/@tanstack/react-query.gen'
import { SandboxServices } from './SandboxServices'
import { AccessSessionsList } from './AccessSessionsList'

export function SandboxVaultAccess({
  projectId,
  sandboxId,
}: {
  projectId?: number | null
  sandboxId: string
}) {
  const [page, setPage] = useState(1)
  const vaults = useQuery(
    sandboxAccessVaultsOptions({
      path: { sandbox_id: sandboxId },
      query: { page, page_size: 20 },
    })
  )
  return (
    <section className="space-y-4" aria-label="Sandbox service access">
      <h2 className="font-semibold">Vault access</h2>
      {vaults.isError ? (
        <Callout tone="error" title="Could not load available vaults">
          <Button onClick={() => void vaults.refetch()}>Retry</Button>
        </Callout>
      ) : vaults.isPending ? (
        <p role="status">Loading available vaults…</p>
      ) : vaults.data?.total === 0 ? (
        <Callout title="Grant access">
          No vault is linked to this sandbox. An instance administrator can link
          a vault directly to this sandbox or to its project. Then approve a
          bundle for a limited time, here or in AI chat.
          <Button asChild variant="outline">
            <Link to="/vault">Set up vault access</Link>
          </Button>
        </Callout>
      ) : (
        vaults.data?.items.map(({ vault }) => (
          <div
            key={vault.id}
            className="flex flex-wrap items-center justify-between gap-2 rounded-lg border p-3"
          >
            <span>{vault.name}</span>
            <Button asChild variant="outline">
              <Link
                to={`/vault/vaults/${encodeURIComponent(vault.id)}?tab=bundles&sandbox=${encodeURIComponent(sandboxId)}`}
              >
                Grant access
              </Link>
            </Button>
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
      <AccessSessionsList sandboxId={sandboxId} />
      {projectId != null && (
        <SandboxServices projectId={projectId} sandboxId={sandboxId} />
      )}
    </section>
  )
}
