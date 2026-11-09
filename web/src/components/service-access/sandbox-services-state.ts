// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { z } from 'zod'

export const approvalExpirySchema = z.object({
  deadline: z
    .string()
    .refine(
      (value) =>
        !value ||
        (Number.isFinite(Date.parse(value)) &&
          Date.parse(value) > Date.now() + 60_000),
      'Choose an expiry more than one minute from now, or leave it empty.'
    ),
})

export function serviceAccessLabel(status: string, connected: boolean): string {
  if (status === 'approved') return connected ? 'Active' : 'Connecting'
  return (
    (
      {
        pending_approval: 'Awaiting approval',
        suspended: 'Suspended',
        revoked: 'Revoked',
        expired: 'Expired',
        policy_changed: 'Policy changed',
        service_disabled: 'Service disabled',
      } as Record<string, string>
    )[status] ?? 'Unavailable'
  )
}
