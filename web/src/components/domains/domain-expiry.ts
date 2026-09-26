// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { isServingCert } from '@/lib/domain-status'

export function hasUrgentCertificate(
  status: string,
  expiration: number | null | undefined,
  now = Date.now()
): boolean {
  return (
    isServingCert(status) &&
    expiration != null &&
    Number.isFinite(expiration) &&
    expiration - now <= 15 * 24 * 60 * 60 * 1000
  )
}
