// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { EgressRecordingStatus } from '@/api/client'

/** What two records look like, for the onboarding state. */
export const EXAMPLE_RECORDS = [
  'CONNECT api.example.com:443 allowed (tunnel, 1.3 KiB sent, 4.5 KiB received, 230 ms)',
  'CONNECT blocked.example.com:443 refused: no_allow_rule',
]

/** What the switch may do: turning recording off is always allowed. */
export function canToggle(
  status: EgressRecordingStatus,
  disabled: boolean
): boolean {
  if (disabled) return false
  return status.enabled || status.configured
}
