// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Info } from 'lucide-react'

/**
 * What the two private ranges on the Worker Nodes page are for. A worker
 * that joined outbound (it dials the control plane, with no address of its
 * own) shows the same mesh address under
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
            private network. Each node gets one address from it, shown under{' '}
            <strong>Mesh</strong>
            {meshReady ? '' : ' once the network is up'}. A worker that joined
            without an address of its own (it dials this server) is reached at
            that address, so <strong>Node address</strong> shows the same one; a
            worker joined at its own address (<code>--private-address</code>)
            shows that one instead.
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
