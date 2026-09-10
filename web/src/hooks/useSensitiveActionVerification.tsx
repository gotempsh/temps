// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  isStepUpRequired,
  oneShotSensitiveActionRetry,
  requiresMfaSetup,
} from '@/lib/sensitiveActionProblem'
import { SensitiveActionVerificationDialog } from '@/components/auth/SensitiveActionVerificationDialog'
import { useCallback, useState } from 'react'

interface PendingVerification {
  retry: () => void
  mfaSetupRequired: boolean
}

/**
 * Converts a backend STEP_UP_REQUIRED response into a verification dialog and
 * retries the original mutation once verification succeeds.
 */
export function useSensitiveActionVerification() {
  const [pending, setPending] = useState<PendingVerification | null>(null)

  const handleSensitiveActionError = useCallback(
    (error: unknown, retry: () => void): boolean => {
      if (!isStepUpRequired(error)) return false
      setPending({
        retry: oneShotSensitiveActionRetry(retry),
        mfaSetupRequired: requiresMfaSetup(error),
      })
      return true
    },
    []
  )

  const verificationDialog = (
    <SensitiveActionVerificationDialog
      open={pending !== null}
      mfaSetupRequired={pending?.mfaSetupRequired ?? false}
      onOpenChange={(open) => {
        if (!open) setPending(null)
      }}
      onVerified={() => {
        const retry = pending?.retry
        setPending(null)
        retry?.()
      }}
    />
  )

  return { handleSensitiveActionError, verificationDialog }
}
