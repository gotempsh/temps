// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback } from 'react'
import { useNavigate } from 'react-router'

/**
 * Back navigation that still works on a deep link.
 *
 * `navigate(-1)` silently does nothing when the page was opened directly — a
 * shared URL, a new tab, a bookmark — because there is no earlier entry in
 * this history session to pop. The user clicks "Back" and stays exactly where
 * they were, with no feedback. Self-hosted users hit this on every link
 * someone pastes them.
 *
 * React Router records its position in the history session as
 * `window.history.state.idx` (it is `0`, or `null` on the very first entry,
 * when there is nothing of ours behind us). When there is nothing to pop, go
 * to `fallback` — which is where the button's label claims it goes anyway.
 *
 * @param fallback Absolute path to use when there's no history to go back to.
 *
 * @example
 * const goBack = useGoBack(`/projects/${project.slug}/traces`)
 * <Button onClick={goBack}>Back to Traces</Button>
 */
export function useGoBack(fallback: string) {
  const navigate = useNavigate()

  return useCallback(() => {
    const idx = (window.history.state as { idx?: number | null } | null)?.idx
    if (typeof idx === 'number' && idx > 0) {
      navigate(-1)
      return
    }
    // `replace` so the dead-end entry doesn't linger in the stack.
    navigate(fallback, { replace: true })
  }, [navigate, fallback])
}
