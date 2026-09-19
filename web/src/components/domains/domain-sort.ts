// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DomainResponse } from '@/api/client/types.gen'

export type DomainSort = 'domain' | 'status' | 'expiration'
export function sortDomains<
  T extends Pick<
    DomainResponse,
    'id' | 'domain' | 'status' | 'expiration_time'
  >,
>(domains: T[], sort: DomainSort, direction: 'asc' | 'desc'): T[] {
  return [...domains].sort((a, b) => {
    let result = 0
    if (sort === 'expiration') {
      const left = a.expiration_time
      const right = b.expiration_time
      const missingLeft = left == null || !Number.isFinite(left)
      const missingRight = right == null || !Number.isFinite(right)
      // Unknown dates stay last in either direction.
      if (missingLeft !== missingRight) return missingLeft ? 1 : -1
      if (!missingLeft && !missingRight) result = left - right
    } else {
      result = a[sort].localeCompare(b[sort])
    }
    return (
      result * (direction === 'asc' ? 1 : -1) ||
      a.domain.localeCompare(b.domain) ||
      a.id - b.id
    )
  })
}
