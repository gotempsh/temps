import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
} from 'react'

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

interface AutofixOnboardingValue {
  isOpen: boolean
  target: AutofixOnboardingTarget | null
  /** Increments on every open so the dialog re-reads readiness fresh. */
  openSeq: number
  open: (target?: AutofixOnboardingTarget) => void
  close: () => void
}

const AutofixOnboardingContext = createContext<AutofixOnboardingValue | null>(
  null
)

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
