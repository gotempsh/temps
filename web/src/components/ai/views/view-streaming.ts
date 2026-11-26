// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createContext, useContext } from 'react'

/**
 * Whether the answer a view belongs to is still being written. A half
 * written view shows a placeholder while it streams and its source once the
 * answer has settled, and only the transcript row knows which it is.
 */
export const ViewStreamingContext = createContext(false)

export function useViewStreaming(): boolean {
  return useContext(ViewStreamingContext)
}
