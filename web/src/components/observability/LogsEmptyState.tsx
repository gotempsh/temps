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
      description="Send structured application logs using OpenTelemetry or a Sentry SDK."
      action={
        <div className="flex flex-wrap justify-center gap-2">
          <Button asChild size="sm" variant="outline">
            <Link to={`/projects/${projectSlug}/traces#traces-setup`}>
              Set up OpenTelemetry
            </Link>
          </Button>
          <Button asChild size="sm" variant="outline">
            <Link to={`/projects/${projectSlug}/errors/setup`}>
              Set up Sentry
            </Link>
          </Button>
        </div>
      }
    />
  )
}
