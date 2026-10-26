// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getServiceLogIngestionOptions,
  getServiceLogIngestionQueryKey,
  updateServiceLogIngestionMutation,
} from '@/api/client/@tanstack/react-query.gen'
import {
  LogIngestionBudgetCard,
  type LogIngestionBudget,
} from '@/components/logs/LogIngestionBudgetCard'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'

/** A managed service's own container log ingestion budget. */
export function ServiceLogIngestionCard({ serviceId }: { serviceId: number }) {
  const queryClient = useQueryClient()
  const current = useQuery(
    getServiceLogIngestionOptions({ path: { id: serviceId } })
  )
  const update = useMutation({
    ...updateServiceLogIngestionMutation(),
    meta: { errorTitle: 'Failed to update log ingestion budget' },
    onSuccess: () =>
      queryClient.invalidateQueries({
        queryKey: getServiceLogIngestionQueryKey({ path: { id: serviceId } }),
      }),
  })

  const handleSave = (budget: LogIngestionBudget) => {
    const saving = update.mutateAsync({
      path: { id: serviceId },
      body: {
        log_ingest_lines_per_sec: budget.lines,
        log_ingest_kib_per_sec: budget.kib,
      },
    })
    toast.promise(saving, {
      loading: 'Updating log ingestion budget...',
      success: 'Log ingestion budget updated',
      error: 'Failed to update log ingestion budget',
    })
    return saving
  }

  return (
    <LogIngestionBudgetCard
      scope="service"
      value={
        current.data
          ? {
              lines: current.data.log_ingest_lines_per_sec,
              kib: current.data.log_ingest_kib_per_sec,
            }
          : undefined
      }
      onSave={handleSave}
      isSaving={update.isPending}
    />
  )
}
