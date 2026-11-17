// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Badge } from '@/components/ui/badge'
import { DeploymentStatus } from './DeploymentStatus'
import { DeploymentResponse } from '@/api/client'

const statusStyles = {
  completed:
    'bg-emerald-50 dark:bg-emerald-950/30 text-emerald-700 dark:text-emerald-400 border-emerald-200 dark:border-emerald-900',
  failed:
    'bg-red-50 dark:bg-red-950/30 text-red-700 dark:text-red-400 border-red-200 dark:border-red-900',
  pending:
    'bg-yellow-50 dark:bg-yellow-950/30 text-yellow-700 dark:text-yellow-400 border-yellow-200 dark:border-yellow-900',
  running:
    'bg-blue-50 dark:bg-blue-950/30 text-blue-700 dark:text-blue-400 border-blue-200 dark:border-blue-900',
}

interface DeploymentStatusBadgeProps {
  deployment: DeploymentResponse
  className?: string
}

export function DeploymentStatusBadge({
  deployment,
  className,
}: DeploymentStatusBadgeProps) {
  return (
    <Badge
      variant="outline"
      className={`shrink-0 whitespace-nowrap tabular-nums ${statusStyles[deployment.status as keyof typeof statusStyles]} ${className ?? ''}`}
    >
      <DeploymentStatus deployment={deployment} />
    </Badge>
  )
}
