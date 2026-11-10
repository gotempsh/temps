// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { Link } from 'react-router'
import { Button } from '@temps-sdk/ui'
import { SandboxServices } from './SandboxServices'
import { AccessSessionsList } from './AccessSessionsList'
import { Callout } from '@temps-sdk/ds'

/** Service approvals and legacy fixed sessions scoped to this sandbox. */
export function SandboxVaultAccess({
  projectId,
  sandboxId,
}: {
  projectId?: number | null
  sandboxId: string
}) {
  return (
    <section className="space-y-4" aria-label="Sandbox service access">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 className="font-semibold">Services</h2>
        <Button variant="outline" asChild>
          <Link to={projectId ? `/vault/projects/${projectId}` : '/vault'}>
            Manage vaults
          </Link>
        </Button>
      </div>
      {projectId ? (
        <>
          <SandboxServices
            key={`${projectId}:${sandboxId}`}
            projectId={projectId}
            sandboxId={sandboxId}
          />
          <details>
            <summary className="cursor-pointer text-sm">
              Previous fixed sessions
            </summary>
            <AccessSessionsList projectId={projectId} sandboxId={sandboxId} />
          </details>
        </>
      ) : (
        <Callout title="A project is required">
          Create a sandbox from a project to grant scoped service access.
        </Callout>
      )}
    </section>
  )
}
