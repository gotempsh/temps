// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { MouseEvent } from 'react'
import { Link } from 'react-router'
import { ArrowUpRight } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import type { SandboxView } from './helpers'

type SandboxNodeFields = Pick<SandboxView, 'node_id' | 'node_name'>

/**
 * Node badge for the sandbox list (ADR-048). Only worker-hosted sandboxes
 * get one: on a single-node install every sandbox is on the control plane
 * and the badge would be noise. The detail page always shows the node.
 */
export function SandboxNodeBadge({
  sandbox,
  canOpenNode,
  onClick,
}: {
  sandbox: SandboxNodeFields
  /** Node pages need settings access; others get a plain badge. */
  canOpenNode: boolean
  onClick?: (e: MouseEvent) => void
}) {
  if (sandbox.node_id == null) return null
  if (!canOpenNode) {
    return (
      <Badge variant="outline" title="Worker node hosting this sandbox">
        {sandbox.node_name}
      </Badge>
    )
  }
  return (
    <Link
      to={`/settings/nodes/${sandbox.node_id}`}
      onClick={onClick}
      title={`Open node ${sandbox.node_name}`}
    >
      <Badge variant="outline" className="gap-0.5 hover:bg-accent">
        {sandbox.node_name}
        <ArrowUpRight className="h-3 w-3" />
      </Badge>
    </Link>
  )
}

/** The "Node" fact on the sandbox detail page. */
export function SandboxNodeValue({
  sandbox,
  canOpenNode,
}: {
  sandbox: SandboxNodeFields
  canOpenNode: boolean
}) {
  if (sandbox.node_id == null) return <>Control plane</>
  if (!canOpenNode) return <>{sandbox.node_name}</>
  return (
    <Link
      to={`/settings/nodes/${sandbox.node_id}`}
      title={`Open node ${sandbox.node_name}`}
      className="inline-flex items-center gap-0.5 underline decoration-muted-foreground/50 underline-offset-2 hover:decoration-foreground"
    >
      {sandbox.node_name}
      <ArrowUpRight className="h-3 w-3" />
    </Link>
  )
}
