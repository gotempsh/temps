// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Badge } from '@/components/ui/badge'
import { Loader2 } from 'lucide-react'
import { cn } from '@/lib/utils'

interface AutopilotStatusBadgeProps {
  status: string
}

const activeStatuses = new Set([
  'cloning',
  'analyzing',
  'fixing',
  'pushing',
  'creating_pr',
  'deploying',
])

export function AutopilotStatusBadge({ status }: AutopilotStatusBadgeProps) {
  if (status === 'pending') {
    return <Badge variant="outline">{status}</Badge>
  }

  if (activeStatuses.has(status)) {
    return (
      <Badge variant="default" className="gap-1">
        <Loader2 className="h-3 w-3 animate-spin" />
        {status.replace('_', ' ')}
      </Badge>
    )
  }

  if (status === 'analyzed') {
    return (
      <Badge
        className={cn(
          'bg-blue-500/10 text-blue-400 border-blue-500/20 hover:bg-blue-500/20'
        )}
      >
        analyzed
      </Badge>
    )
  }

  if (status === 'fix_ready') {
    return (
      <Badge
        className={cn(
          'bg-purple-500/10 text-purple-400 border-purple-500/20 hover:bg-purple-500/20'
        )}
      >
        fix ready
      </Badge>
    )
  }

  if (status === 'completed') {
    return (
      <Badge
        className={cn(
          'bg-green-500/10 text-green-500 border-green-500/20 hover:bg-green-500/20'
        )}
      >
        completed
      </Badge>
    )
  }

  if (status === 'no_fix') {
    return (
      <Badge
        className={cn(
          'bg-yellow-500/10 text-yellow-500 border-yellow-500/20 hover:bg-yellow-500/20'
        )}
      >
        no fix
      </Badge>
    )
  }

  if (status === 'failed') {
    return <Badge variant="destructive">failed</Badge>
  }

  if (status === 'cancelled') {
    return (
      <Badge
        className={cn(
          'bg-orange-500/10 text-orange-500 border-orange-500/20 hover:bg-orange-500/20'
        )}
      >
        cancelled
      </Badge>
    )
  }

  return <Badge variant="outline">{status}</Badge>
}
