// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProblemDetails } from '@/api/client'
import { problemDetail } from '@/lib/api-problem'
import {
  announceMutationFailure,
  type MutationErrorContext,
} from '@/lib/mutation-error-toast'

/**
 * Handle a failed service creation. A taken name belongs on the name field;
 * every other refusal (an unreachable cluster placement, a missing image, a
 * node that cannot run the service, ...) is announced with the server's
 * reason. A per-mutation `onError` replaces the app-wide one, so without the
 * announcement those refusals were silent.
 */
export function handleServiceCreateError(
  error: unknown,
  context: MutationErrorContext | undefined,
  onNameConflict: (message: string) => void
): void {
  if (
    error &&
    typeof error === 'object' &&
    'error_code' in error &&
    error.error_code === 'SERVICE_NAME_CONFLICT'
  ) {
    onNameConflict(problemDetail(error, 'Choose a unique service name.'))
    return
  }
  announceMutationFailure(error as ProblemDetails, context)
}
