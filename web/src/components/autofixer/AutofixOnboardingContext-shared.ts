// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createContext, useContext } from 'react'

/**
 * Target a specific project so the dialog can also check the git gate and
 * deep-link its CTA to that project's settings. Omit for a global open
 * (provider + sandbox gates only).
 */
export interface AutofixOnboardingTarget {
  projectId?: number
  projectSlug?: string
  /**
   * The project's git state. A repo linked by public URL has `hasRepo` true
   * but `connected` false — readable, but autofix can't open a PR with it.
   */
  projectGit?: {
    connected: boolean
    hasRepo: boolean
    label?: string
  }
}

export interface AutofixOnboardingValue {
  isOpen: boolean
  target: AutofixOnboardingTarget | null
  /** Increments on every open so the dialog re-reads readiness fresh. */
  openSeq: number
  open: (target?: AutofixOnboardingTarget) => void
  close: () => void
}

export const AutofixOnboardingContext =
  createContext<AutofixOnboardingValue | null>(null)

/**
 * Trigger and read the app-wide autofix onboarding dialog. Any component under
 * the provider can call `openOnboarding(target?)` to walk the user through
 * setup, from anywhere in the console.
 */
export function useAutofixOnboarding() {
  const ctx = useContext(AutofixOnboardingContext)
  if (!ctx) {
    throw new Error(
      'useAutofixOnboarding must be used within an AutofixOnboardingProvider'
    )
  }
  return {
    isOpen: ctx.isOpen,
    target: ctx.target,
    openSeq: ctx.openSeq,
    openOnboarding: ctx.open,
    closeOnboarding: ctx.close,
  }
}
