// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Info } from 'lucide-react'

/**
 * What the two private ranges on the Worker Nodes page are for. A worker
 * that joined over the private network shows the same machine address under
 * "Node address" and "Mesh", and its containers use a second range, which
 * users read as a collision. They cannot overlap: the cluster refuses a
 * container range that overlaps the mesh pool (`PoolOverlapsMesh`).
 */
export function NodeAddressRanges({
  meshCidr,
  meshReady = true,
  computeCidr,
  subnetPrefixLen,
}: {
  meshCidr: string | null | undefined
  /** False while the private network starts: nodes have no address yet. */
  meshReady?: boolean
  computeCidr: string | null | undefined
  subnetPrefixLen: number | null | undefined
}) {
  if (!meshCidr && !computeCidr) return null
  return (
    <div className="flex gap-2 rounded-md border bg-muted/30 p-3 text-xs text-muted-foreground">
      <Info className="mt-0.5 h-4 w-4 shrink-0" />
      <div className="space-y-1">
        {meshCidr && (
          <p>
            <code className="font-mono">{meshCidr}</code>: machines on the
            private network. Each node gets one address; it{' '}
            {meshReady ? 'shows' : 'will show'} under{' '}
            <strong>Node address</strong> and <strong>Mesh</strong>, which are
            the same for a worker that joined over the private network.
          </p>
        )}
        {computeCidr && (
          <p>
            <code className="font-mono">{computeCidr}</code>: container
            addresses
            {subnetPrefixLen ? <>, a /{subnetPrefixLen} per node</> : null}.
          </p>
        )}
        {meshCidr && computeCidr && (
          <p>
            The two ranges never overlap: Temps refuses a container range that
            overlaps the private network.
          </p>
        )}
      </div>
    </div>
  )
}
