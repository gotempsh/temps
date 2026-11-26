// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { isServingCert } from '@/lib/domain-status'

export function hasUrgentCertificate(
  status: string,
  expiration: string | null | undefined,
  now = Date.now()
): boolean {
  const expiresAt = expiration == null ? NaN : Date.parse(expiration)
  return (
    isServingCert(status) &&
    Number.isFinite(expiresAt) &&
    expiresAt - now <= 15 * 24 * 60 * 60 * 1000
  )
}
