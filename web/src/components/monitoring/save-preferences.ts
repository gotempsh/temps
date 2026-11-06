// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { UpdatePreferencesData } from '@/api/client'
import { updatePreferences } from '@/api/client/sdk.gen'
import { getErrorMessage } from '@/utils/errorHandling'
import { toast } from 'sonner'

export interface PreferencesSaveMessages {
  /** Shown while the request runs ("Saving domain alert settings..."). */
  loading: string
  /** Shown when the server accepted the change. */
  success: string
  /** What failed ("domain alert settings"); the server's reason follows. */
  subject: string
}

/**
 * Saves notification preferences with a toast that follows the request. The
 * call throws on an error status, so a refused save reads "Failed to save ...:
 * <reason>" instead of the success message it used to show.
 */
export async function savePreferences(
  body: UpdatePreferencesData['body'],
  messages: PreferencesSaveMessages
): Promise<boolean> {
  const request = updatePreferences({ body, throwOnError: true })
  toast.promise(request, {
    loading: messages.loading,
    success: messages.success,
    error: (error: unknown) =>
      `Failed to save ${messages.subject}: ${getErrorMessage(error)}`,
  })
  try {
    await request
    return true
  } catch {
    // Reported by the toast above.
    return false
  }
}
