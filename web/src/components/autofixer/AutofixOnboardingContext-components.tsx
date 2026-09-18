// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useMemo, useState } from 'react'
import {
  type AutofixOnboardingTarget,
  AutofixOnboardingContext,
} from './AutofixOnboardingContext-shared'

/**
 * App-level state for the AI autofix onboarding dialog. Held above the router
 * (rendered once in the app shell) so any surface — the per-error autofix card,
 * an errors-list empty state, a settings nudge — can call `openOnboarding()`
 * without each mounting its own dialog. Mirrors the AI assistant dock pattern.
 */
export function AutofixOnboardingProvider({
  children,
}: {
  children: React.ReactNode
}) {
  const [isOpen, setIsOpen] = useState(false)
  const [target, setTarget] = useState<AutofixOnboardingTarget | null>(null)
  const [openSeq, setOpenSeq] = useState(0)

  const open = useCallback((t?: AutofixOnboardingTarget) => {
    setTarget(t ?? null)
    setOpenSeq((n) => n + 1)
    setIsOpen(true)
  }, [])

  const close = useCallback(() => setIsOpen(false), [])

  const value = useMemo(
    () => ({ isOpen, target, openSeq, open, close }),
    [isOpen, target, openSeq, open, close]
  )

  return (
    <AutofixOnboardingContext.Provider value={value}>
      {children}
    </AutofixOnboardingContext.Provider>
  )
}
