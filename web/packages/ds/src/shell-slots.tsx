// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createContext, useContext, type ReactNode } from 'react'

/**
 * Slots the console shell exposes to the screen inside it. A screen's
 * PageTitle puts its name into the header breadcrumb; a screen's StatusLine
 * puts its verdict behind the header's attention indicator. Both use portals,
 * so the screen keeps owning the content and the shell only owns the place.
 * Outside a shell (docs pages, demos) both components render inline.
 */
export type ShellSlots = { crumb: HTMLElement | null; attention: HTMLElement | null }
export const ShellSlotsContext = createContext<ShellSlots | null>(null)
export function ShellSlotsProvider({ value, children }: { value: ShellSlots; children: ReactNode }) {
  return <ShellSlotsContext.Provider value={value}>{children}</ShellSlotsContext.Provider>
}
export const useShellSlots = () => useContext(ShellSlotsContext)
