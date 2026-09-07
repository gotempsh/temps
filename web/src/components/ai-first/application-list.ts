// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ApplicationResponse } from '@/api/client'

export function applicationsFromPages(
  pages: ApplicationResponse[][],
  selected?: ApplicationResponse
): ApplicationResponse[] {
  const seen = new Set<string>()
  return [...pages.flat(), ...(selected ? [selected] : [])].filter(
    (application) => {
      if (seen.has(application.public_id)) return false
      seen.add(application.public_id)
      return true
    }
  )
}

export function nextApplicationPage(
  lastPage: readonly ApplicationResponse[],
  loadedPageCount: number,
  pageSize: number
): number | undefined {
  return lastPage.length === pageSize ? loadedPageCount + 1 : undefined
}

export function resolveApplicationSelection(
  applications: Pick<ApplicationResponse, 'public_id'>[],
  applicationFromUrl: string | null,
  currentApplicationId: string | null,
  globalScope: boolean
): string | null {
  if (globalScope) return null
  if (
    applicationFromUrl &&
    applications.some(
      (application) => application.public_id === applicationFromUrl
    )
  ) {
    return applicationFromUrl
  }
  if (
    currentApplicationId &&
    applications.some(
      (application) => application.public_id === currentApplicationId
    )
  ) {
    return currentApplicationId
  }
  return applications[0]?.public_id ?? null
}
