// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Bot } from 'lucide-react'
import { Link } from 'react-router'
import { Button } from '@/components/ui/button'
import { EmptyState } from '@/components/ui/empty-state'

export function AiActivityEmptyState({ setupHref }: { setupHref: string }) {
  return (
    <EmptyState
      size="compact"
      icon={Bot}
      title="No AI traces yet"
      description="Connect OpenTelemetry to see model calls, token usage, and agent activity."
      action={
        <Button asChild size="sm">
          <Link to={setupHref}>Configure telemetry</Link>
        </Button>
      }
    />
  )
}
