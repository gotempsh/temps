// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { cancelRun } from '@/api/client/sdk.gen'
import { getErrorMessage } from '@/utils/errorHandling'
import { useMutation } from '@tanstack/react-query'
import { toast } from 'sonner'

/**
 * Cancels an Autopilot run. `onCancelled` runs only once the server has
 * cancelled it; a refusal or a failed request says why and leaves the page
 * (and the run) as they are. The page used to post with a raw fetch, ignore
 * the response and reload, so a refused cancel looked like it had worked.
 */
export function useCancelRun(
  projectId: number,
  runId: number,
  onCancelled: () => void
) {
  return useMutation({
    mutationFn: () =>
      cancelRun({
        path: { project_id: projectId, run_id: runId },
        throwOnError: true,
      }),
    onSuccess: () => onCancelled(),
    // Its own handler, so the reason is shown even without an app-wide one.
    onError: (error) =>
      toast.error('Failed to cancel run', {
        description: getErrorMessage(error, 'The run is still running.'),
      }),
  })
}
