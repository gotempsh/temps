// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { RefetchOptions } from '@tanstack/react-query'
import { useEffect } from 'react'

/**
 * Refetch a query in the background every time its panel is shown.
 *
 * A tab panel that stays mounted while hidden keeps its cached data, and a
 * `refetchInterval` only restarts its timer when the panel comes back, so
 * without this the panel shows what it saw when it was last open until the
 * next tick. The cached data stays on screen while the refetch runs, and a
 * fetch already in flight (the first one on mount) is reused, not repeated.
 */
export function useRefetchWhenShown(
  refetch: (options?: RefetchOptions) => Promise<unknown>,
  shown: boolean
) {
  useEffect(() => {
    if (shown) void refetch({ cancelRefetch: false })
  }, [refetch, shown])
}
