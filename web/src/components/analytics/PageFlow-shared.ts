// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectResponse } from '@/api/client/types.gen'

export interface PageFlowProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

export function durationSortValue(seconds: number | null | undefined) {
  return seconds != null && seconds > 0 ? seconds : null
}
