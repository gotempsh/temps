// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ReadFailure } from '@/components/ui/read-failure'

/**
 * Shown by a settings page in place of its form when the settings could not
 * be read. A refused read (a role without `settings:read`) is explained as
 * such and has no Retry, which could only be refused again.
 */
export function SettingsLoadFailure({
  error,
  onRetry,
}: {
  error: unknown
  onRetry?: () => void
}) {
  return (
    <ReadFailure resource="Platform settings" error={error} onRetry={onRetry} />
  )
}
