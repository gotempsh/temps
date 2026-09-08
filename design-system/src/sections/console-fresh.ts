// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useSearchParams } from 'react-router'

/**
 * "Fresh install" for the sandbox: `/v1?fresh=1` renders every screen as the
 * console looks minutes after `temps serve` first started, with nothing
 * configured and nothing recorded. It is a route, not a header control — the
 * console header carries console features only, and a demo state that lives
 * in the URL is linkable, bookmarkable and screenshot-stable. Screens that
 * have no first-run state yet simply ignore it.
 */
export function useFresh(): boolean {
  const [params] = useSearchParams()
  return params.get('fresh') === '1'
}
