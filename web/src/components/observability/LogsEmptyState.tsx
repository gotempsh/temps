// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ScrollText } from 'lucide-react'
import { Link } from 'react-router'
import { Button } from '@/components/ui/button'
import { EmptyState } from '@/components/ui/empty-state'

export function LogsEmptyState({ projectSlug }: { projectSlug: string }) {
  return (
    <EmptyState
      size="compact"
      icon={ScrollText}
      title="No telemetry logs yet"
      description="Connect OpenTelemetry to search structured application logs here."
      action={
        <Button asChild size="sm">
          <Link to={`/projects/${projectSlug}/traces#traces-setup`}>
            Connect telemetry
          </Link>
        </Button>
      }
    />
  )
}
