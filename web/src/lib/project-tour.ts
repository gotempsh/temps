// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useSyncExternalStore, type CSSProperties } from 'react'

export const PROJECT_TOUR_EVENT = 'temps:project-tour'

let tourActive = false
const tourActiveListeners = new Set<() => void>()

export function setProjectTourActive(value: boolean) {
  tourActive = value
  for (const listener of tourActiveListeners) listener()
}

export function isProjectTourActive() {
  return tourActive
}

export function useProjectTourActive() {
  return useSyncExternalStore(
    (listener) => {
      tourActiveListeners.add(listener)
      return () => tourActiveListeners.delete(listener)
    },
    () => tourActive
  )
}

export function isProjectTourHomePage(
  slug: string | undefined,
  pathname: string
): boolean {
  return (
    !!slug &&
    (pathname === `/projects/${slug}` ||
      pathname === `/projects/${slug}/project`)
  )
}

export function getProjectTourNavigationTarget({
  active,
  slug,
  route,
  lastTarget,
}: {
  active: boolean
  slug: string | undefined
  route: string
  lastTarget: string | null
}): string | null {
  if (!active || !slug) return null

  const target = `/projects/${slug}/${route}`
  return target === lastTarget ? null : target
}

const CARD_WIDTH = 320
const CARD_EST_HEIGHT = 180
const CARD_GUTTER = 16

interface TourAnchorPosition {
  top: number
  right: number
}

export function getProjectTourCardStyle({
  isMobile,
  anchor,
  viewportWidth,
  viewportHeight,
}: {
  isMobile: boolean
  anchor: TourAnchorPosition | null
  viewportWidth: number
  viewportHeight: number
}): CSSProperties {
  if (isMobile) {
    return {
      right: CARD_GUTTER,
      bottom: CARD_GUTTER,
      left: CARD_GUTTER,
    }
  }

  const maxLeft = Math.max(12, viewportWidth - CARD_WIDTH - 12)
  const maxTop = Math.max(12, viewportHeight - CARD_EST_HEIGHT - 12)

  return {
    top: Math.min(Math.max(12, anchor?.top ?? 88), maxTop),
    left: Math.min(Math.max(12, anchor ? anchor.right + 12 : 24), maxLeft),
  }
}
