// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import { updateProjectSettingsMutation } from '@/api/client/@tanstack/react-query.gen'
import {
  LogIngestionBudgetCard,
  type LogIngestionBudget,
} from '@/components/logs/LogIngestionBudgetCard'
import { useMutation } from '@tanstack/react-query'
import { toast } from 'sonner'

/** This project's own container log ingestion budget. */
export function LogIngestionCard({
  project,
  refetch,
}: {
  project: ProjectResponse
  refetch: () => void
}) {
  const updateProjectSettings = useMutation({
    ...updateProjectSettingsMutation(),
    meta: { errorTitle: 'Failed to update log ingestion budget' },
  })

  const handleSave = async (budget: LogIngestionBudget) => {
    await toast.promise(
      updateProjectSettings.mutateAsync({
        path: { project_id: project.id },
        body: {
          // Explicit null returns the field to the platform budget.
          log_ingest_lines_per_sec: budget.lines,
          log_ingest_kib_per_sec: budget.kib,
        },
      }),
      {
        loading: 'Updating log ingestion budget...',
        success: 'Log ingestion budget updated',
        error: 'Failed to update log ingestion budget',
      }
    )
    refetch()
  }

  return (
    <LogIngestionBudgetCard
      scope="project"
      value={{
        lines: project.log_ingest_lines_per_sec,
        kib: project.log_ingest_kib_per_sec,
      }}
      onSave={handleSave}
      isSaving={updateProjectSettings.isPending}
    />
  )
}
