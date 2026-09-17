// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { type ReactNode } from 'react'
import { type ShellSlots, ShellSlotsContext } from './shell-slots-shared'

export function ShellSlotsProvider({
  value,
  children,
}: {
  value: ShellSlots
  children: ReactNode
}) {
  return (
    <ShellSlotsContext.Provider value={value}>
      {children}
    </ShellSlotsContext.Provider>
  )
}
