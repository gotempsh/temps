// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import { latestRunForSourceOptions } from '@/api/client/@tanstack/react-query.gen'
import { AutofixRunConfigForm } from '@/components/autofixer/AutofixRunConfigForm'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { useQuery } from '@tanstack/react-query'
import { Wand2 } from 'lucide-react'
import { useState } from 'react'
import { Navigate, useParams } from 'react-router'

interface AutofixRedirectProps {
  project: ProjectResponse
}

/**
 * Entry point for `/errors/:errorGroupId/autofix`. If a run already exists
 * for this error group, redirects to the unified agent run viewer at
 * `/agents/:runId`. Otherwise shows the pre-run configuration (harness,
 * model, max turns, branch, notes) — or the provider credential setup path
 * when nothing is configured yet — and starts the analysis on submit.
 */
export function AutofixRedirect({ project }: AutofixRedirectProps) {
  const { errorGroupId } = useParams<{ errorGroupId: string }>()
  const groupId = Number(errorGroupId) || 0
  const [newRunId, setNewRunId] = useState<number | null>(null)

  const { data: latestRun, isLoading } = useQuery({
    ...latestRunForSourceOptions({
      path: { project_id: project.id },
      query: {
        trigger_source_type: 'error_group',
        trigger_source_id: groupId,
      },
    }),
    enabled: groupId > 0,
    retry: false,
  })

  if (groupId <= 0) {
    return <Navigate to="../errors" replace />
  }

  const targetRunId = latestRun?.id ?? newRunId
  if (targetRunId) {
    return <Navigate to={`../agents/${targetRunId}`} replace />
  }

  if (isLoading) {
    return (
      <div className="space-y-4">
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-40 w-full" />
      </div>
    )
  }

  // latestRunForSource returns 404 when no run exists — show the run config.
  return (
    <div className="max-w-2xl mx-auto">
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-base">
            <Wand2 className="h-4 w-4" />
            Fix with AI
          </CardTitle>
          <CardDescription>
            An AI agent clones the repository, traces the error to its root
            cause, and proposes a fix as a pull request. Review the settings
            below, then start the analysis.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <AutofixRunConfigForm
            project={project}
            errorGroupId={groupId}
            onStarted={setNewRunId}
          />
        </CardContent>
      </Card>
    </div>
  )
}
