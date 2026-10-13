// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Badge } from '@/components/ui/badge'
import { NODE_ROLE_BADGE, nodeSchedulingRole } from '@/lib/node-role'

/**
 * Badge for a node's scheduling role (`temps.sh/role=dedicated|builder`).
 * Renders nothing for an ordinary worker.
 */
export function NodeRoleBadge({
  labels,
  className,
}: {
  labels: unknown
  className?: string
}) {
  const role = nodeSchedulingRole(labels)
  if (!role) return null
  const { label, description } = NODE_ROLE_BADGE[role]
  return (
    <Badge
      variant={role === 'dedicated' ? 'default' : 'secondary'}
      className={className ?? 'text-[10px]'}
      title={description}
    >
      {label}
    </Badge>
  )
}
