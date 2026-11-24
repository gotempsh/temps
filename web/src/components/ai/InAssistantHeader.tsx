// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useContext, type ReactNode } from 'react'
import { createPortal } from 'react-dom'

import { AssistantHeaderSlot } from './assistant-header-slot'

/**
 * Render `children` in the conversation header when there is one, or in
 * place otherwise, so the tools never disappear.
 */
export function InAssistantHeader({ children }: { children: ReactNode }) {
  const slot = useContext(AssistantHeaderSlot)
  if (!slot) {
    return (
      <div className="flex shrink-0 items-center justify-end gap-0.5">
        {children}
      </div>
    )
  }
  return createPortal(children, slot)
}
