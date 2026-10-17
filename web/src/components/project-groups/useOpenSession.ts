// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'

/**
 * A counter that advances each time `trigger` changes to a truthy value:
 * a dialog's `open` flag, or the record it was opened for. Use it as the
 * `key` of the component that owns the dialog's form state, so every opening
 * starts from fresh state instead of resetting fields in an effect.
 *
 * Closing leaves the counter alone, so a dialog that is animating shut keeps
 * showing what the user last saw instead of flashing back to empty fields.
 */
export function useOpenSession(trigger: unknown): number {
  const [state, setState] = useState({ trigger, session: 0 })
  if (state.trigger !== trigger) {
    // Adjusting state while rendering: React re-renders immediately, before
    // anything is committed, so no stale session is ever painted.
    setState({
      trigger,
      session: trigger ? state.session + 1 : state.session,
    })
    return trigger ? state.session + 1 : state.session
  }
  return state.session
}
