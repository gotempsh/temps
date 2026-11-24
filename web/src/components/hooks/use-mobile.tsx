// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as React from 'react'

const MOBILE_BREAKPOINT = 768

function subscribe(onStoreChange: () => void) {
  const mediaQuery = window.matchMedia(
    `(max-width: ${MOBILE_BREAKPOINT - 1}px)`
  )
  mediaQuery.addEventListener('change', onStoreChange)
  return () => mediaQuery.removeEventListener('change', onStoreChange)
}

function getSnapshot() {
  return window.innerWidth < MOBILE_BREAKPOINT
}

function getServerSnapshot() {
  return false
}

export function useIsMobile() {
  return React.useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot)
}

const DESKTOP_BREAKPOINT = 1024

function subscribeDesktop(onStoreChange: () => void) {
  const mediaQuery = window.matchMedia(`(min-width: ${DESKTOP_BREAKPOINT}px)`)
  mediaQuery.addEventListener('change', onStoreChange)
  return () => mediaQuery.removeEventListener('change', onStoreChange)
}

function getDesktopSnapshot() {
  return window.innerWidth >= DESKTOP_BREAKPOINT
}

/** Matches Tailwind's `lg` breakpoint. */
export function useIsDesktop() {
  return React.useSyncExternalStore(
    subscribeDesktop,
    getDesktopSnapshot,
    getServerSnapshot
  )
}
