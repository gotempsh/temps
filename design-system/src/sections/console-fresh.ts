// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useSearchParams } from 'react-router'

/**
 * "Fresh install" for the sandbox: `?fresh=1` renders every screen as the
 * console looks minutes after `temps serve` first started, with nothing
 * configured and nothing recorded. It is a demo control, toggled from the
 * shell header, so every first-run state stays one click away instead of a
 * URL trick. Screens that have no first-run state yet simply ignore it.
 */
export function useFresh(): [boolean, (on: boolean) => void] {
  const [params, setParams] = useSearchParams()
  const fresh = params.get('fresh') === '1'
  const set = (on: boolean) => { const p = new URLSearchParams(params); if (on) p.set('fresh', '1'); else p.delete('fresh'); setParams(p) }
  return [fresh, set]
}
