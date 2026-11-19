// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Code2, ScrollText } from 'lucide-react'
import { Link } from 'react-router'
import { Button } from '@/components/ui/button'
import { EmptyState } from '@/components/ui/empty-state'
import {
  CopySetupPromptButton,
  PlainHttpSetup,
} from '@/components/project/setup/PlainHttpSetup'

export function LogsEmptyState({
  project,
}: {
  project: { id: number; name: string; slug: string }
}) {
  const [showHttp, setShowHttp] = useState(false)
  const projectSlug = project.slug
  return (
    <div className="space-y-6">
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
            <Button
              size="sm"
              variant="outline"
              aria-expanded={showHttp}
              onClick={() => setShowHttp((v) => !v)}
            >
              <Code2 className="size-4" />
              Plain HTTP / any framework
            </Button>
            <CopySetupPromptButton project={project} signal="logs" />
          </div>
        }
      />
      {showHttp && (
        <div className="rounded-lg border bg-card p-4">
          <PlainHttpSetup project={project} signal="logs" />
        </div>
      )}
    </div>
  )
}
