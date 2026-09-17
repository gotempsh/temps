// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createContext, useContext } from 'react'

export type BreadcrumbItem = {
  label: string
  href?: string
}

export type BreadcrumbContextType = {
  breadcrumbs: BreadcrumbItem[]
  setBreadcrumbs: (items: BreadcrumbItem[]) => void
}

export const BreadcrumbContext = createContext<
  BreadcrumbContextType | undefined
>(undefined)

export function useBreadcrumbs() {
  const context = useContext(BreadcrumbContext)
  if (context === undefined) {
    throw new Error('useBreadcrumbs must be used within a BreadcrumbProvider')
  }
  return context
}
