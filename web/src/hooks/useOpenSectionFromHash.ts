// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { useLocation } from 'react-router'

/**
 * Opens the settings section a link points at (`…#shield-entries`) and
 * brings it into view. Only the ids given are honoured, so an unrelated hash
 * never opens a section. Pass `ready` false while the page has not rendered
 * its sections yet.
 */
export function useOpenSectionFromHash(ids: readonly string[], ready = true) {
  const { hash } = useLocation()
  const id = hash.slice(1)
  const known = ids.includes(id)
  useEffect(() => {
    if (known && ready) openSettingsSection(id)
  }, [id, known, ready])
}

/** Opens the settings section with this DOM id and brings it into view. */
export function openSettingsSection(id: string): boolean {
  const element = document.getElementById(id)
  if (!(element instanceof HTMLDetailsElement)) return false
  element.open = true
  element.scrollIntoView({ block: 'start' })
  return true
}
