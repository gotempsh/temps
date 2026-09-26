// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type SecurityCheckStatusFilter = 'all' | 'enabled' | 'disabled'

export function filterComposeSecurityChecks<
  T extends { id: string; group: string; label: string; consequence: string },
>(
  checks: readonly T[],
  disabledChecks: readonly string[],
  search: string,
  status: SecurityCheckStatusFilter
): T[] {
  const query = search.trim().toLowerCase()
  const disabled = new Set(disabledChecks)

  return checks.filter((check) => {
    const isDisabled = disabled.has(check.id)
    if (status === 'enabled' && isDisabled) return false
    if (status === 'disabled' && !isDisabled) return false
    return `${check.id} ${check.group} ${check.label} ${check.consequence}`
      .toLowerCase()
      .includes(query)
  })
}
