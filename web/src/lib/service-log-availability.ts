// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type ServiceLogAvailability = 'checking' | 'available' | 'needs-project'

export function serviceLogAvailability({
  linksLoading,
  linksFailed,
  linkedProjectCount,
}: {
  linksLoading: boolean
  linksFailed: boolean
  linkedProjectCount: number | undefined
}): ServiceLogAvailability {
  if (linksLoading) return 'checking'
  // If the prerequisite check itself fails, let the log endpoint return its
  // canonical error instead of incorrectly claiming that the service is
  // unlinked.
  if (linksFailed || linkedProjectCount === undefined) return 'available'
  return linkedProjectCount === 0 ? 'needs-project' : 'available'
}
